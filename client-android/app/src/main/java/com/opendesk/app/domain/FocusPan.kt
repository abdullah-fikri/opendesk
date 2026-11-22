package com.opendesk.app.domain

/**
 * Keeps the host's focused text field visible while a keyboard covers the bottom of the viewer: where the viewport
 * should go (pan, and zoom for a tiny field) so the field sits just above the keyboard, and the in-between steps of
 * the move. Pure math, unit-tested.
 */
object FocusPan {

    /** The focused field on the remote frame, as fractions (0..1) of it (`text-focus` `normX/Y/W/H`). */
    data class Field(val x: Float, val y: Float, val w: Float, val h: Float) {
        companion object {
            /** A usable field from a `text-focus` event, or null (no rect, or an empty one). */
            fun of(x: Float?, y: Float?, w: Float?, h: Float?): Field? {
                if (x == null || y == null || w == null || h == null) return null
                if (!(w > 0f && h > 0f) || x.isNaN() || y.isNaN()) return null
                return Field(x.coerceIn(0f, 1f), y.coerceIn(0f, 1f), w.coerceAtMost(1f), h.coerceAtMost(1f))
            }
        }
    }

    /** A viewport position: zoom and pan (see [Viewport]). */
    data class State(val zoom: Float, val panX: Float, val panY: Float) {
        /** Same place within `eps` view pixels (and a hair of zoom). */
        fun near(other: State, eps: Float = 1.5f): Boolean =
            kotlin.math.abs(zoom - other.zoom) < 0.01f &&
                kotlin.math.abs(panX - other.panX) <= eps &&
                kotlin.math.abs(panY - other.panY) <= eps
    }

    fun state(v: Viewport) = State(v.zoom, v.panX, v.panY)

    /** Zoom used for a field shown smaller than the minimum height. */
    const val MIN_ZOOM = 1.5f

    /** Zoom never goes higher than this for a tiny field. */
    const val MAX_AUTO_ZOOM = 3f

    /**
     * Where `viewport` should go so `field` is visible above a keyboard whose top edge is at `keyboardTop` (view
     * pixels; the view's height when nothing covers it): its bottom `margin` pixels above the keyboard, or its top
     * `margin` pixels below the view's top when it is above the view. A field shown less than `minFieldHeight`
     * pixels high is zoomed to at least [MIN_ZOOM] (enough to reach the minimum, at most [MAX_AUTO_ZOOM]). A field
     * already in view stays put; one taller than the room above the keyboard moves only if none of it shows.
     * Horizontally the field is brought in view; a field wider than the view gets its left edge (where text starts)
     * `margin` from the left. `viewport` is not changed; the result respects its pan limits with the keyboard's
     * inset (see [Viewport.bottomInset]).
     */
    fun target(
        viewport: Viewport,
        field: Field,
        keyboardTop: Float,
        margin: Float,
        minFieldHeight: Float,
        extraRoom: Boolean = true,
    ): State {
        val v = viewport.copy()
        val kbTop = keyboardTop.coerceIn(0f, v.viewHeight)
        v.bottomInset = if (extraRoom) inset(v.viewHeight, kbTop, margin) else 0f

        val shownHeight = field.h * v.drawnHeight
        if (shownHeight > 0f && shownHeight < minFieldHeight) {
            val needed = v.zoom * minFieldHeight / shownHeight
            val zoom = maxOf(v.zoom, needed.coerceIn(MIN_ZOOM, MAX_AUTO_ZOOM))
            if (zoom > v.zoom) {
                v.zoomBy(zoom / v.zoom, v.toViewX(field.x + field.w / 2), v.toViewY(field.y + field.h / 2))
            }
        }

        val left = v.toViewX(field.x)
        val right = v.toViewX(field.x + field.w)
        val dx = when {
            right - left > v.viewWidth - 2 * margin -> margin - left
            left < margin -> margin - left
            right > v.viewWidth - margin -> v.viewWidth - margin - right
            else -> 0f
        }

        val top = v.toViewY(field.y)
        val bottom = v.toViewY(field.y + field.h)
        val low = kbTop - margin
        val room = low - margin
        val dy = if (bottom - top <= room) {
            when {
                bottom > low -> low - bottom
                top < margin -> margin - top
                else -> 0f
            }
        } else {
            // Taller than the room: keep it as it is while any of it shows, else bring its top in.
            if (top >= low || bottom <= margin) margin - top else 0f
        }
        v.panBy(dx, dy)
        return state(v)
    }

    /**
     * Pan room to give the viewport ([Viewport.bottomInset]) while a keyboard's top edge is at `keyboardTop`: the
     * frame's bottom may rise to `margin` above the keyboard, so even a field at the very bottom gets its gap.
     */
    fun inset(viewHeight: Float, keyboardTop: Float, margin: Float): Float =
        (viewHeight - keyboardTop + margin).coerceIn(0f, viewHeight)

    /** The state `t` (0..1) of the way from `from` to `to`. */
    fun lerp(from: State, to: State, t: Float): State = State(
        from.zoom + (to.zoom - from.zoom) * t,
        from.panX + (to.panX - from.panX) * t,
        from.panY + (to.panY - from.panY) * t,
    )
}
