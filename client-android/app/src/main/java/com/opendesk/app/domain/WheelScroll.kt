package com.opendesk.app.domain

import kotlin.math.abs
import kotlin.math.pow

/**
 * Turns finger travel on the phone into wheel deltas for the computer, so the remote page follows the finger
 * instead of jumping a whole notch per few pixels.
 *
 * Travel is measured in remote screen pixels (finger pixels divided by the frame's scale), so zooming in scrolls
 * finer. Deltas go out in [STEP] units (a third of a notch, one line in Windows): hosts pass 40 and more through as
 * wheel units (Windows) or add them up into notches (Linux), while values under 40 would be read as whole notches.
 */
class WheelScroll(private val unitsPerRemotePx: Float = UNITS_PER_REMOTE_PX) {
    private var rest = 0f

    /** Adds `remotePx` of travel (positive = finger down = page up) and returns the delta to send, or 0. */
    fun add(remotePx: Float): Int {
        if (rest != 0f && (rest > 0f) != (remotePx > 0f)) rest = 0f
        rest += remotePx * unitsPerRemotePx
        val steps = (rest / STEP).toInt()
        rest -= steps * STEP
        return steps * STEP
    }

    fun reset() {
        rest = 0f
    }

    companion object {
        /** Smallest delta sent: one line. */
        const val STEP = 40

        /**
         * Wheel units per remote pixel of finger travel. Browsers scroll about 100 px per notch (120 units), so 1.2
         * keeps the page under the finger.
         */
        const val UNITS_PER_REMOTE_PX = 1.2f

        /** Velocity kept per 16 ms frame while a flick coasts. */
        const val FRICTION = 0.94f

        /** A flick slower than this (px/s on the phone) does not coast, and coasting stops below it. */
        const val MIN_FLING_PX_S = 250f

        /** Velocity after `dtMs` of coasting from `v`. */
        fun decay(v: Float, dtMs: Float): Float = v * FRICTION.pow(dtMs / 16f)

        fun coasting(v: Float): Boolean = abs(v) >= MIN_FLING_PX_S
    }
}
