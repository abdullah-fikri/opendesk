package com.opendesk.app.ui.viewer

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Rect
import com.opendesk.app.domain.FramedVideo
import com.opendesk.app.domain.SwapChain

/**
 * Keeps the remote screen as a mutable bitmap and applies the host's video messages to it in order: legacy plain
 * JPEGs and framed key frames replace it, framed deltas move regions inside it and blit patches from their JPEG
 * atlas (the counterpart of core-engine/src/codec/delta.rs). One decoder thread only.
 *
 * Nothing is allocated per frame: key frames and atlases decode into the bitmaps of the previous ones
 * (`inBitmap`), moves go through one scratch bitmap, and the screen gets one of a few pooled display buffers
 * ([snapshot]) into which only what changed since that buffer was last shown is copied.
 */
class FrameCompositor {
    enum class Result { UPDATED, UNCHANGED, NEED_KEY }

    /** A display buffer: what the screen draws while the next message is applied to the frame. */
    class Buffer(val bitmap: Bitmap) {
        internal val canvas = Canvas(bitmap)

        /** Part of the frame that changed since this buffer was last filled (all of it when new). */
        internal val stale = Rect(0, 0, bitmap.width, bitmap.height)
    }

    private val options = BitmapFactory.Options().apply {
        inPreferredConfig = Bitmap.Config.RGB_565
        inMutable = true
    }
    private var frame: Bitmap? = null
    private var canvas: Canvas? = null
    private var lastSeq: Long? = null
    private val src = Rect()
    private val dst = Rect()

    /** Last atlas, reused for the next one's pixels. */
    private var atlas: Bitmap? = null

    /** Moves copy through it, since source and destination may overlap. */
    private var scratch: Bitmap? = null
    private var scratchCanvas: Canvas? = null

    private val chain = SwapChain<Buffer>(DISPLAY_BUFFERS)

    /** Every display buffer that exists, to mark changes in. Decoder thread only. */
    private val buffers = ArrayList<Buffer>(DISPLAY_BUFFERS)

    /** Applies one video message. */
    fun apply(msg: ByteArray): Result {
        if (!FramedVideo.isFramed(msg)) {
            lastSeq = null
            return if (replace(msg, 0, 0, 0)) Result.UPDATED else Result.UNCHANGED
        }
        val header = FramedVideo.parse(msg) ?: return invalidate()
        return when (header.kind) {
            FramedVideo.KIND_KEY -> if (replace(msg, header.payloadOffset, header.width, header.height)) {
                lastSeq = header.seq
                Result.UPDATED
            } else {
                invalidate()
            }
            FramedVideo.KIND_DELTA -> applyDelta(msg, header)
            // H.264 is never sent to a viewer without `video-h264`; resynchronize if it is.
            else -> invalidate()
        }
    }

    /**
     * The current frame in a display buffer for the screen, which keeps drawing it while the next message is
     * applied. Hand it to [shown] once it is on screen, or to [giveBack] if it never will be. Null when there is no
     * frame yet or every buffer is in use.
     */
    fun snapshot(): Buffer? {
        val fb = frame ?: return null
        val buffer = chain.take(
            fits = { it.bitmap.width == fb.width && it.bitmap.height == fb.height },
            create = { Buffer(Bitmap.createBitmap(fb.width, fb.height, Bitmap.Config.RGB_565)).also { buffers.add(it) } },
            // Free, so neither on screen nor waiting for it.
            drop = {
                buffers.remove(it)
                it.bitmap.recycle()
            },
        ) ?: return null
        if (!buffer.stale.isEmpty) {
            buffer.canvas.drawBitmap(fb, buffer.stale, buffer.stale, null)
            buffer.stale.setEmpty()
        }
        return buffer
    }

    /** Main thread: `buffer` is on screen. */
    fun shown(buffer: Buffer) = chain.shown(buffer)

    /** A buffer from [snapshot] that will not be shown. */
    fun giveBack(buffer: Buffer) = chain.giveBack(buffer)

    private fun changed(left: Int, top: Int, right: Int, bottom: Int) {
        for (b in buffers) b.stale.union(left, top, right, bottom)
    }

    private fun applyDelta(msg: ByteArray, h: FramedVideo.Frame): Result {
        val fb = frame
        val c = canvas
        if (fb == null || c == null || !FramedVideo.follows(lastSeq, h.seq) || h.width != fb.width || h.height != fb.height) {
            return invalidate()
        }
        lastSeq = h.seq
        for (m in h.moves) {
            val r = FramedVideo.clip(m, fb.width, fb.height, fb.width, fb.height) ?: continue
            val sc = scratchFor(fb)
            src.set(r.srcX, r.srcY, r.srcX + r.w, r.srcY + r.h)
            dst.set(0, 0, r.w, r.h)
            sc.drawBitmap(fb, src, dst, null)
            src.set(dst)
            dst.set(r.dstX, r.dstY, r.dstX + r.w, r.dstY + r.h)
            c.drawBitmap(scratch!!, src, dst, null)
            changed(dst.left, dst.top, dst.right, dst.bottom)
        }
        val payloadSize = msg.size - h.payloadOffset
        if (h.patches.isEmpty() || payloadSize <= 0) {
            return if (h.moves.isEmpty()) Result.UNCHANGED else Result.UPDATED
        }
        val decoded = decode(msg, h.payloadOffset, atlas) ?: return invalidate()
        if (decoded !== atlas) {
            atlas?.recycle()
            atlas = decoded
        }
        for (p in h.patches) {
            val r = FramedVideo.clip(p, decoded.width, decoded.height, fb.width, fb.height) ?: continue
            src.set(r.srcX, r.srcY, r.srcX + r.w, r.srcY + r.h)
            dst.set(r.dstX, r.dstY, r.dstX + r.w, r.dstY + r.h)
            c.drawBitmap(decoded, src, dst, null)
            changed(dst.left, dst.top, dst.right, dst.bottom)
        }
        return Result.UPDATED
    }

    /** The scratch canvas, as large as the frame. */
    private fun scratchFor(fb: Bitmap): Canvas {
        val s = scratch
        val sc = scratchCanvas
        if (s != null && sc != null && s.width == fb.width && s.height == fb.height) return sc
        s?.recycle()
        val fresh = Bitmap.createBitmap(fb.width, fb.height, Bitmap.Config.RGB_565)
        scratch = fresh
        return Canvas(fresh).also { scratchCanvas = it }
    }

    /** Key frame of `width` x `height` (0 when unknown: a legacy plain JPEG). */
    private fun replace(msg: ByteArray, offset: Int, width: Int, height: Int): Boolean {
        val old = frame
        // Same size: decode straight into the frame (the screen draws its own buffers, never this one).
        val reuse = old?.takeIf { width > 0 && it.width == width && it.height == height }
        val decoded = decode(msg, offset, reuse) ?: return false
        if (decoded !== old) old?.recycle()
        frame = decoded
        canvas = Canvas(decoded)
        changed(0, 0, decoded.width, decoded.height)
        return true
    }

    /** Decodes the JPEG at `offset`, into `into` when it can hold it. */
    private fun decode(msg: ByteArray, offset: Int, into: Bitmap?): Bitmap? {
        val length = msg.size - offset
        if (length <= 0) return null
        options.inBitmap = into?.takeIf { it.isMutable && !it.isRecycled }
        return try {
            BitmapFactory.decodeByteArray(msg, offset, length, options)
        } catch (_: IllegalArgumentException) {
            // `into` cannot take this picture (too small or another format): decode into a new bitmap.
            options.inBitmap = null
            BitmapFactory.decodeByteArray(msg, offset, length, options)
        } finally {
            options.inBitmap = null
        }
    }

    private fun invalidate(): Result {
        lastSeq = null
        return Result.NEED_KEY
    }

    private companion object {
        /** One on screen, one just replaced (the screen may still draw it), one being filled. */
        const val DISPLAY_BUFFERS = 3
    }
}
