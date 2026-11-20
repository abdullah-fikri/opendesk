package com.opendesk.app.domain

import kotlin.math.PI
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.hypot
import kotlin.math.sin

/**
 * Geometry of the viewer's radial menu: `count` items on a half circle of [radius] around a center, opening away
 * from the screen edge it was pulled from (left for the right edge, right for the left edge). Pure math in view
 * pixels so placement and picking are unit-tested.
 *
 * Angles are degrees, counter-clockwise from "right" with y pointing up (so 90 is straight up). Items on a
 * right-edge menu run from the top (90) through the left (180) to the bottom (270).
 */
class RadialLayout(
    val count: Int,
    val radius: Float,
    /** Radius of one item's circle. */
    val itemRadius: Float,
    /** Picking needs the finger at least this far from the center; closer is "nothing" (cancel / back). */
    val deadZone: Float,
    /** The menu opens to the right (pulled from the left edge) instead of to the left. */
    val opensRight: Boolean = false,
) {
    var centerX = 0f
        private set
    var centerY = 0f
        private set

    /** Centers the menu on the finger at (x, y), kept far enough from the view's edges that every item shows. */
    fun place(x: Float, y: Float, viewW: Float, viewH: Float, margin: Float) {
        val reach = radius + itemRadius + margin
        centerX = if (opensRight) x.coerceIn(margin, (viewW - reach).coerceAtLeast(margin))
        else x.coerceIn(reach.coerceAtMost(viewW - margin), viewW - margin)
        centerY = y.coerceIn(reach.coerceAtMost(viewH / 2f), (viewH - reach).coerceAtLeast(viewH / 2f))
    }

    /** Angle of item `i`, spread evenly over the half circle with half a slot free at each end. */
    fun angle(i: Int): Float {
        val a = 90f + 180f * (i + 0.5f) / count.coerceAtLeast(1)
        return if (opensRight) 180f - a else a
    }

    fun itemX(i: Int, r: Float = radius): Float = centerX + r * cos(rad(angle(i)))

    fun itemY(i: Int, r: Float = radius): Float = centerY - r * sin(rad(angle(i)))

    /** The item the finger at (x, y) points at, or -1 inside the dead zone or behind the half circle. */
    fun pick(x: Float, y: Float): Int {
        val dx = x - centerX
        val dy = centerY - y
        if (hypot(dx, dy) < deadZone) return -1
        var a = Math.toDegrees(atan2(dy.toDouble(), dx.toDouble())).toFloat()
        if (opensRight) a = 180f - a
        if (a < 0f) a += 360f
        // Behind the half circle (toward the edge): a little slack past each end still picks the end item.
        if (a < 90f - SLACK_DEG || a > 270f + SLACK_DEG) return -1
        val n = count.coerceAtLeast(1)
        return ((a - 90f) / 180f * n).toInt().coerceIn(0, n - 1)
    }

    private fun rad(deg: Float): Float = (deg * PI / 180.0).toFloat()

    private companion object {
        const val SLACK_DEG = 20f
    }
}
