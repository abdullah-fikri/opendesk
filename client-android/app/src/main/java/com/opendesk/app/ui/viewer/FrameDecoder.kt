package com.opendesk.app.ui.viewer

import android.graphics.Bitmap
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import java.util.concurrent.atomic.AtomicReference

/**
 * Applies video messages on its own thread, in order: framed deltas build on the frame before them, so none may
 * be skipped. The socket thread queues them (at most [MAX_QUEUED]; beyond that the queue is dropped and a key
 * frame requested). At most one bitmap at a time waits for the main thread, always the newest.
 *
 * `render` gets the bitmap and the measured frame rate on the main thread; `requestKeyFrame` is called on the main
 * thread, at most every [KEY_REQUEST_INTERVAL_MS].
 */
class FrameDecoder(private val render: (Bitmap, Int) -> Unit, private val requestKeyFrame: () -> Unit) {
    private val main = Handler(Looper.getMainLooper())
    private val lock = Object()
    private val queue = ArrayDeque<ByteArray>()
    private var running = false

    /** Bumped by every start, so a thread left over from a quick stop/start exits instead of doubling up. */
    private var generation = 0
    private var lastKeyRequestMs = 0L

    // Newest frame waiting for the main thread; replaced, never queued.
    private val decoded = AtomicReference<Pair<Bitmap, Int>?>(null)

    /** Socket thread. */
    fun offer(bytes: ByteArray) {
        val overflow = synchronized(lock) {
            val full = queue.size >= MAX_QUEUED
            if (full) queue.clear()
            queue.addLast(bytes)
            lock.notifyAll()
            full
        }
        if (overflow) askKeyFrame()
    }

    fun start() {
        val gen = synchronized(lock) {
            if (running) return
            running = true
            queue.clear()
            ++generation
        }
        Thread({ loop(gen) }, "OpenDesk-FrameDecoder").apply {
            priority = Thread.MAX_PRIORITY
            start()
        }
        // A new screen has no base frame for the deltas that may follow.
        askKeyFrame()
    }

    fun stop() {
        synchronized(lock) {
            running = false
            queue.clear()
            lock.notifyAll()
        }
    }

    private fun askKeyFrame() {
        val now = SystemClock.uptimeMillis()
        if (now - lastKeyRequestMs < KEY_REQUEST_INTERVAL_MS) return
        lastKeyRequestMs = now
        main.post(requestKeyFrame)
    }

    private fun loop(gen: Int) {
        val compositor = FrameCompositor()
        var frames = 0
        var windowStart = SystemClock.uptimeMillis()
        var fps = 0
        while (true) {
            val (bytes, more) = synchronized(lock) {
                while (running && generation == gen && queue.isEmpty()) {
                    try {
                        lock.wait()
                    } catch (_: InterruptedException) {
                        return
                    }
                }
                if (!running || generation != gen) return
                queue.removeFirst() to queue.isNotEmpty()
            }
            when (compositor.apply(bytes)) {
                FrameCompositor.Result.NEED_KEY -> {
                    askKeyFrame()
                    continue
                }
                FrameCompositor.Result.UNCHANGED -> continue
                FrameCompositor.Result.UPDATED -> {}
            }
            frames++
            val now = SystemClock.uptimeMillis()
            if (now - windowStart >= 1000L) {
                fps = ((frames * 1000f) / (now - windowStart)).toInt().coerceAtLeast(1)
                frames = 0
                windowStart = now
            }
            // Copy only what the screen can show: skip while newer messages wait, or a render is pending.
            if (more || decoded.get() != null) continue
            val bitmap = compositor.snapshot() ?: continue
            if (decoded.getAndSet(bitmap to fps) == null) {
                main.post { decoded.getAndSet(null)?.let { (b, f) -> render(b, f) } }
            }
        }
    }

    private companion object {
        /** Video messages waiting for the decoder; more means it fell behind, and a key frame resynchronizes. */
        const val MAX_QUEUED = 8
        const val KEY_REQUEST_INTERVAL_MS = 500L
    }
}
