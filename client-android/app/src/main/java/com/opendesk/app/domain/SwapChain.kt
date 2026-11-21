package com.opendesk.app.domain

/**
 * A small set of display buffers handed back and forth between the decoder thread and the screen, so no frame
 * allocates one. The decoder [take]s a free buffer, fills it and posts it; the main thread calls [shown] once it
 * is on screen. A buffer becomes free again only when the one *after* the next is shown: the screen may still be
 * drawing the previous picture while the newest is set, so it is never written to during that frame.
 *
 * With three buffers the decoder always finds one while no frame waits for the main thread (one shown, one just
 * replaced, one free). Thread-safe; pure so the accounting is unit-tested.
 */
class SwapChain<T : Any>(private val capacity: Int = 3) {
    private val free = ArrayDeque<T>()
    private var current: T? = null
    private var previous: T? = null
    private var created = 0

    /**
     * A free buffer that [fits], or a new one from [create] while fewer than `capacity` exist; null when every
     * buffer is in use. Free buffers that no longer fit (the frame size changed) go to [drop] and are replaced.
     */
    @Synchronized
    fun take(fits: (T) -> Boolean, create: () -> T, drop: (T) -> Unit = {}): T? {
        while (free.isNotEmpty()) {
            val item = free.removeFirst()
            if (fits(item)) return item
            created--
            drop(item)
        }
        if (created >= capacity) return null
        created++
        return create()
    }

    /** Main thread: `item` is on screen now; the buffer shown before the previous one is free again. */
    @Synchronized
    fun shown(item: T) {
        if (item === current) return
        previous?.let { free.addLast(it) }
        previous = current
        current = item
    }

    /** A taken buffer that will not be shown after all. */
    @Synchronized
    fun giveBack(item: T) {
        if (item !== current && item !== previous) free.addLast(item)
    }

    /** Buffers that exist (free, waiting or on screen). */
    @get:Synchronized
    val size: Int get() = created

    /** Buffers ready to be written. */
    @get:Synchronized
    val freeCount: Int get() = free.size
}
