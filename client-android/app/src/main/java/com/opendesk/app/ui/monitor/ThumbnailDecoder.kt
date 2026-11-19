package com.opendesk.app.ui.monitor

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import com.opendesk.app.domain.Thumbnail
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Decodes monitor-wall thumbnails on one worker thread and hands them to the main thread in batches, at most
 * [MAX_UPDATES_PER_SECOND] times a second (as on the desktop). Only the newest frame of each host is kept: every
 * thumbnail is a whole JPEG, so dropping older ones is harmless and a slow phone never queues up.
 */
class ThumbnailDecoder(private val onDecoded: (Map<String, Bitmap>) -> Unit) {
    private val thread = HandlerThread("monitor-decode").apply { start() }
    private val worker = Handler(thread.looper)
    private val main = Handler(Looper.getMainLooper())
    private val pending = ConcurrentHashMap<String, Thumbnail>()
    private val scheduled = AtomicBoolean(false)
    private var lastPassMs = 0L

    @Volatile
    private var closed = false

    private val pass = Runnable { decodePending() }

    /** Socket thread: keeps `frame` as its host's newest. */
    fun offer(frame: Thumbnail) {
        if (closed) return
        pending[frame.hostClean] = frame
        if (scheduled.compareAndSet(false, true)) {
            val wait = (lastPassMs + MIN_PASS_MS - SystemClock.uptimeMillis()).coerceAtLeast(0L)
            worker.postDelayed(pass, wait)
        }
    }

    /** Stops decoding; frames still pending are dropped. */
    fun close() {
        closed = true
        pending.clear()
        worker.removeCallbacksAndMessages(null)
        main.removeCallbacksAndMessages(null)
        thread.quitSafely()
    }

    private fun decodePending() {
        scheduled.set(false)
        lastPassMs = SystemClock.uptimeMillis()
        val out = HashMap<String, Bitmap>()
        for (id in pending.keys.toList()) {
            val frame = pending.remove(id) ?: continue
            val options = BitmapFactory.Options().apply { inPreferredConfig = Bitmap.Config.RGB_565 }
            BitmapFactory.decodeByteArray(frame.bytes, frame.offset, frame.length, options)?.let { out[id] = it }
        }
        if (out.isNotEmpty() && !closed) main.post { if (!closed) onDecoded(out) }
    }

    private companion object {
        const val MAX_UPDATES_PER_SECOND = 10
        const val MIN_PASS_MS = 1000L / MAX_UPDATES_PER_SECOND
    }
}
