package com.opendesk.app.ui.viewer

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Rect
import com.opendesk.app.domain.FramedVideo

/**
 * Keeps the remote screen as a mutable bitmap and applies the host's video messages to it in order: legacy plain
 * JPEGs and framed key frames replace it, framed deltas move regions inside it and blit patches from their JPEG
 * atlas (the counterpart of core-engine/src/codec/delta.rs). One decoder thread only.
 */
class FrameCompositor {
    enum class Result { UPDATED, UNCHANGED, NEED_KEY }

    private val options = BitmapFactory.Options().apply {
        inPreferredConfig = Bitmap.Config.RGB_565
        inMutable = true
    }
    private var frame: Bitmap? = null
    private var canvas: Canvas? = null
    private var lastSeq: Long? = null
    private val src = Rect()
    private val dst = Rect()

    /** Applies one video message. */
    fun apply(msg: ByteArray): Result {
        if (!FramedVideo.isFramed(msg)) {
            lastSeq = null
            return if (replace(msg, 0)) Result.UPDATED else Result.UNCHANGED
        }
        val header = FramedVideo.parse(msg) ?: return invalidate()
        return when (header.kind) {
            FramedVideo.KIND_KEY -> if (replace(msg, header.payloadOffset)) {
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

    /** A copy of the current frame for the screen, which keeps drawing it while the next message is applied. */
    fun snapshot(): Bitmap? = frame?.let { it.copy(it.config ?: Bitmap.Config.RGB_565, false) }

    private fun applyDelta(msg: ByteArray, h: FramedVideo.Frame): Result {
        val fb = frame
        val c = canvas
        if (fb == null || c == null || !FramedVideo.follows(lastSeq, h.seq) || h.width != fb.width || h.height != fb.height) {
            return invalidate()
        }
        lastSeq = h.seq
        for (m in h.moves) {
            val r = FramedVideo.clip(m, fb.width, fb.height, fb.width, fb.height) ?: continue
            // Copy first: source and destination may overlap.
            val region = Bitmap.createBitmap(fb, r.srcX, r.srcY, r.w, r.h)
            c.drawBitmap(region, r.dstX.toFloat(), r.dstY.toFloat(), null)
            region.recycle()
        }
        val payloadSize = msg.size - h.payloadOffset
        if (h.patches.isEmpty() || payloadSize <= 0) {
            return if (h.moves.isEmpty()) Result.UNCHANGED else Result.UPDATED
        }
        val atlas = BitmapFactory.decodeByteArray(msg, h.payloadOffset, payloadSize, options) ?: return invalidate()
        for (p in h.patches) {
            val r = FramedVideo.clip(p, atlas.width, atlas.height, fb.width, fb.height) ?: continue
            src.set(r.srcX, r.srcY, r.srcX + r.w, r.srcY + r.h)
            dst.set(r.dstX, r.dstY, r.dstX + r.w, r.dstY + r.h)
            c.drawBitmap(atlas, src, dst, null)
        }
        atlas.recycle()
        return Result.UPDATED
    }

    private fun replace(msg: ByteArray, offset: Int): Boolean {
        val decoded = BitmapFactory.decodeByteArray(msg, offset, msg.size - offset, options) ?: return false
        frame?.recycle()
        frame = decoded
        canvas = Canvas(decoded)
        return true
    }

    private fun invalidate(): Result {
        lastSeq = null
        return Result.NEED_KEY
    }
}
