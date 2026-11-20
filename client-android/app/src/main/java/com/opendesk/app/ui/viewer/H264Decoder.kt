package com.opendesk.app.ui.viewer

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface
import com.opendesk.app.domain.FramedVideo
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Hardware H.264 for the remote screen: the host's access units (framed kind 3, sent while the screen is in motion)
 * are decoded by MediaCodec straight onto [surface], with no bitmap in between. The host leaves H.264 with a full
 * JPEG key frame, which the bitmap path shows on top again.
 *
 * [decode] runs on the frame decoder thread; MediaCodec's callbacks run on a thread of their own and hand every
 * decoded picture to the surface at once (no frame is held back for display timing). [failed] is called when the
 * codec cannot be set up or reports an error, so the app can stop advertising H.264.
 */
class H264Decoder(private val failed: () -> Unit) {

    /** Where pictures go; set on the main thread when the screen's texture exists, null when it is gone. */
    @Volatile
    var surface: Surface? = null
        set(value) {
            field = value
            if (value == null) release()
        }

    private val lock = Object()
    private var codec: MediaCodec? = null
    private var codecSurface: Surface? = null
    private var width = 0
    private var height = 0
    private var lastSeq: Long? = null
    private var thread: HandlerThread? = null
    private val inputs = LinkedBlockingQueue<Int>()

    /** Queues one access unit; false when the host must send an IDR first (or the codec is unusable). */
    fun decode(msg: ByteArray, f: FramedVideo.Frame): Boolean {
        val target = surface ?: return false
        val idr = FramedVideo.isIdr(f)
        synchronized(lock) {
            val ready = codec != null && codecSurface === target && width == f.width && height == f.height
            if (!idr && (!ready || !FramedVideo.follows(lastSeq, f.seq))) return false
            if (idr && !ready && !configure(msg, f, target)) return false
        }
        val index = inputs.poll(INPUT_WAIT_MS, TimeUnit.MILLISECONDS) ?: return false.also { lastSeq = null }
        synchronized(lock) {
            val c = codec ?: return false
            return try {
                val buffer = c.getInputBuffer(index) ?: return false
                val length = msg.size - f.payloadOffset
                if (length <= 0 || length > buffer.capacity()) {
                    c.queueInputBuffer(index, 0, 0, 0, 0)
                    lastSeq = null
                    return false
                }
                buffer.clear()
                buffer.put(msg, f.payloadOffset, length)
                c.queueInputBuffer(index, 0, length, System.nanoTime() / 1000, if (idr) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0)
                lastSeq = f.seq
                true
            } catch (e: IllegalStateException) {
                releaseLocked()
                false
            }
        }
    }

    /** Frees the codec and its callback thread (the screen is closing). */
    fun close() {
        release()
        thread?.quitSafely()
        thread = null
    }

    /** A JPEG key frame took over: the next access unit must be an IDR. */
    fun invalidate() {
        synchronized(lock) { lastSeq = null }
    }

    /** Frees the codec (screen hidden or gone); the next IDR sets it up again. */
    fun release() {
        synchronized(lock) { releaseLocked() }
    }

    private fun configure(msg: ByteArray, f: FramedVideo.Frame, target: Surface): Boolean {
        releaseLocked()
        val name = Support.decoderName ?: return false
        return try {
            val format = MediaFormat.createVideoFormat(MIME, f.width, f.height).apply {
                FramedVideo.parameterSets(msg, f.payloadOffset)?.let { (sps, pps) ->
                    setByteBuffer("csd-0", java.nio.ByteBuffer.wrap(sps))
                    setByteBuffer("csd-1", java.nio.ByteBuffer.wrap(pps))
                }
                setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, (f.width * f.height).coerceAtLeast(MIN_INPUT_BYTES))
                // Decode each picture as soon as it arrives: this is a live screen, not a movie.
                setInteger(MediaFormat.KEY_PRIORITY, 0)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            }
            val worker = thread ?: HandlerThread("OpenDesk-H264").also {
                it.start()
                thread = it
            }
            val c = MediaCodec.createByCodecName(name)
            c.setCallback(Callbacks(c), Handler(worker.looper))
            c.configure(format, target, null, 0)
            inputs.clear()
            c.start()
            codec = c
            codecSurface = target
            width = f.width
            height = f.height
            lastSeq = null
            true
        } catch (e: Exception) {
            releaseLocked()
            failed()
            false
        }
    }

    private fun releaseLocked() {
        val c = codec
        codec = null
        codecSurface = null
        lastSeq = null
        inputs.clear()
        if (c != null) {
            try {
                c.stop()
            } catch (_: Exception) {
            }
            c.release()
        }
    }

    private inner class Callbacks(private val owner: MediaCodec) : MediaCodec.Callback() {
        override fun onInputBufferAvailable(c: MediaCodec, index: Int) {
            if (codec === owner) inputs.offer(index)
        }

        override fun onOutputBufferAvailable(c: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            try {
                c.releaseOutputBuffer(index, info.size > 0)
            } catch (_: IllegalStateException) {
                // Released meanwhile.
            }
        }

        override fun onError(c: MediaCodec, e: MediaCodec.CodecException) {
            if (codec !== owner) return
            release()
            if (!e.isTransient) failed()
        }

        override fun onOutputFormatChanged(c: MediaCodec, format: MediaFormat) = Unit
    }

    /** Whether this phone has a hardware H.264 decoder (software decoding would be slower than the JPEG path). */
    object Support {
        val decoderName: String? by lazy {
            runCatching {
                MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.firstOrNull { info ->
                    !info.isEncoder && info.supportedTypes.any { it.equals(MIME, ignoreCase = true) } && isHardware(info)
                }?.name
            }.getOrNull()
        }

        private fun isHardware(info: MediaCodecInfo): Boolean =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                info.isHardwareAccelerated && !info.isSoftwareOnly
            } else {
                val n = info.name.lowercase()
                !n.startsWith("omx.google.") && !n.startsWith("c2.android.") && !n.contains(".sw.")
            }
    }

    private companion object {
        const val MIME = MediaFormat.MIMETYPE_VIDEO_AVC
        const val INPUT_WAIT_MS = 200L
        const val MIN_INPUT_BYTES = 1 shl 20
    }
}
