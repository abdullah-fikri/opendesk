package com.opendesk.app.domain

/**
 * Where the remote frame is drawn inside the view: fit-center at zoom 1, magnified around a focus point above it.
 * All coordinates are view pixels; `pan` is the offset of the zoomed image from its fit-center position.
 * Pure math so the touch-to-remote mapping is unit-tested.
 */
class Viewport {
    var viewWidth = 1f
        private set
    var viewHeight = 1f
        private set
    var imageWidth = 0
        private set
    var imageHeight = 0
        private set
    var zoom = 1f
        private set
    private var panX = 0f
    private var panY = 0f

    /** Size of the view or of the remote frame changed. Keeps zoom and clamps the pan. */
    fun resize(viewW: Int, viewH: Int, imageW: Int, imageH: Int) {
        viewWidth = viewW.coerceAtLeast(1).toFloat()
        viewHeight = viewH.coerceAtLeast(1).toFloat()
        imageWidth = imageW
        imageHeight = imageH
        clampPan()
    }

    /** Scale from frame pixels to view pixels, including zoom. */
    val scale: Float
        get() = fitScale() * zoom

    /** Left edge of the drawn frame in view pixels. */
    val left: Float
        get() = (viewWidth - drawnWidth) / 2f + panX

    /** Top edge of the drawn frame in view pixels. */
    val top: Float
        get() = (viewHeight - drawnHeight) / 2f + panY

    val drawnWidth: Float
        get() = if (imageWidth > 0) imageWidth * scale else viewWidth * zoom

    val drawnHeight: Float
        get() = if (imageHeight > 0) imageHeight * scale else viewHeight * zoom

    /** Remote position (0..1) under view point (x, y), clamped to the frame. */
    fun toNormX(x: Float): Float = ((x - left) / drawnWidth.coerceAtLeast(1f)).coerceIn(0f, 1f)

    fun toNormY(y: Float): Float = ((y - top) / drawnHeight.coerceAtLeast(1f)).coerceIn(0f, 1f)

    /** View position of remote position (0..1). */
    fun toViewX(normX: Float): Float = left + normX * drawnWidth

    fun toViewY(normY: Float): Float = top + normY * drawnHeight

    /** Multiplies the zoom by `factor` (clamped to 1..[MAX_ZOOM]) keeping the point under (focusX, focusY) fixed. */
    fun zoomBy(factor: Float, focusX: Float, focusY: Float) {
        val newZoom = (zoom * factor).coerceIn(1f, MAX_ZOOM)
        if (newZoom == zoom) return
        val nx = (focusX - left) / drawnWidth.coerceAtLeast(1f)
        val ny = (focusY - top) / drawnHeight.coerceAtLeast(1f)
        zoom = newZoom
        // Choose the pan so the same frame point stays under the focus.
        panX += focusX - (left + nx * drawnWidth)
        panY += focusY - (top + ny * drawnHeight)
        clampPan()
    }

    /** Moves the zoomed frame by (dx, dy) view pixels. */
    fun panBy(dx: Float, dy: Float) {
        panX += dx
        panY += dy
        clampPan()
    }

    /** Pans just enough that remote position (normX, normY) is at least `margin` pixels inside the view. */
    fun ensureVisible(normX: Float, normY: Float, margin: Float) {
        val x = toViewX(normX)
        val y = toViewY(normY)
        val m = margin.coerceAtMost(minOf(viewWidth, viewHeight) / 2f)
        if (x < m) panX += m - x else if (x > viewWidth - m) panX -= x - (viewWidth - m)
        if (y < m) panY += m - y else if (y > viewHeight - m) panY -= y - (viewHeight - m)
        clampPan()
    }

    /** Back to fit-center. */
    fun reset() {
        zoom = 1f
        panX = 0f
        panY = 0f
    }

    private fun fitScale(): Float {
        if (imageWidth <= 0 || imageHeight <= 0) return 1f
        return minOf(viewWidth / imageWidth, viewHeight / imageHeight)
    }

    /** A frame larger than the view may not leave a gap at an edge; a smaller one stays centered. */
    private fun clampPan() {
        val maxX = ((drawnWidth - viewWidth) / 2f).coerceAtLeast(0f)
        val maxY = ((drawnHeight - viewHeight) / 2f).coerceAtLeast(0f)
        panX = panX.coerceIn(-maxX, maxX)
        panY = panY.coerceIn(-maxY, maxY)
    }

    companion object {
        const val MAX_ZOOM = 5f
    }
}
