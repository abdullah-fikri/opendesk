package com.opendesk.app.domain

/**
 * What the phone asks the host to stream (`set-resolution` in automatic mode, see docs/protocol.md): the size of
 * its own display, in landscape like the computer screen, so the host never encodes more pixels than the phone
 * can show. The short side is capped at [MAX_SHORT_SIDE] (the host's default 1080 preset): a phone screen is too
 * small for more to be visible, and a larger stream only costs decode time and data.
 *
 * Hosts fit the box to their monitor's aspect ratio and never upscale; hosts that predate automatic mode read only
 * `mode` (1080), their default anyway.
 */
object StreamRequest {
    const val MAX_SHORT_SIDE = 1080

    /** Smallest box worth asking for (hosts clamp to 320x180 as well). */
    private const val MIN_SHORT_SIDE = 180

    /** The box for a display of `width` x `height` physical pixels in any orientation; null when unknown. */
    fun box(width: Int, height: Int): Pair<Int, Int>? {
        if (width <= 0 || height <= 0) return null
        var long = maxOf(width, height)
        var short = minOf(width, height)
        if (short > MAX_SHORT_SIDE) {
            long = (long.toLong() * MAX_SHORT_SIDE / short).toInt()
            short = MAX_SHORT_SIDE
        }
        if (short < MIN_SHORT_SIDE) return null
        // Even sizes, like the host's encoder wants them.
        return (long and 1.inv()) to (short and 1.inv())
    }
}
