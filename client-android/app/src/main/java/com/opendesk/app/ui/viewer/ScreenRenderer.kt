package com.opendesk.app.ui.viewer

import android.graphics.Bitmap
import android.graphics.Matrix
import android.view.TextureView
import android.view.View
import android.widget.ImageView
import com.opendesk.app.domain.Viewport

/**
 * Draws the remote frame with the current fit, zoom and pan, and places the virtual cursor over it.
 *
 * Two layers show the same frame: [video] (under) receives H.264 pictures straight from the hardware decoder while
 * the host streams motion, [screen] (over) shows the JPEG bitmap otherwise. The bitmap layer turns transparent
 * once the first H.264 picture is on screen and opaque again with the next JPEG frame, so switching never shows a
 * blank frame. Both use the same viewport transform.
 */
class ScreenRenderer(
    private val screen: ImageView,
    private val video: TextureView,
    private val cursor: View,
    private val input: RemoteInput,
) {
    val viewport = Viewport()
    private val matrix = Matrix()
    private val videoMatrix = Matrix()

    /** Size of the remote frame (either layer's). */
    private var frameWidth = 0
    private var frameHeight = 0

    /** H.264 pictures are what the host streams now (set when an access unit was decoded, cleared by a JPEG). */
    private var videoActive = false

    init {
        cursor.visibility = View.VISIBLE
        screen.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> layout() }
    }

    /** Shows a decoded JPEG frame over the video layer; a frame of a new size recomputes the fit. */
    fun show(bitmap: Bitmap) {
        videoActive = false
        screen.setImageBitmap(bitmap)
        screen.imageAlpha = OPAQUE
        setFrameSize(bitmap.width, bitmap.height)
    }

    /** The host streams H.264 pictures of `width` x `height` to the video layer. */
    fun showVideo(width: Int, height: Int) {
        videoActive = true
        setFrameSize(width, height)
    }

    /** A new picture reached the video layer: reveal it if H.264 is still what the host streams. */
    fun onVideoPicture() {
        // The bitmap layer stays in place (transparent) so it keeps receiving the touches.
        if (videoActive && screen.imageAlpha != 0) screen.imageAlpha = 0
    }

    /** Fill (cover the view) or fit; keeps the zoom. */
    fun setFill(fill: Boolean) {
        viewport.fill = fill
        apply()
    }

    /** Back to the whole screen. */
    fun fit() {
        viewport.reset()
        apply()
    }

    /** View or frame size changed: recompute the fit and keep zoom and pan within bounds. */
    fun layout() {
        viewport.resize(screen.width, screen.height, frameWidth, frameHeight)
        apply()
    }

    /** Applies the viewport to both layers after a zoom or pan. */
    fun apply() {
        matrix.setScale(viewport.scale, viewport.scale)
        matrix.postTranslate(viewport.left, viewport.top)
        screen.imageMatrix = matrix
        // A TextureView stretches its picture over the whole view first; undo that, then place it like the bitmap.
        val vw = video.width.coerceAtLeast(1).toFloat()
        val vh = video.height.coerceAtLeast(1).toFloat()
        videoMatrix.setScale(viewport.drawnWidth / vw, viewport.drawnHeight / vh)
        videoMatrix.postTranslate(viewport.left, viewport.top)
        video.setTransform(videoMatrix)
        updateCursor()
    }

    fun updateCursor() {
        // The cursor and the frame share the container's padded origin.
        cursor.translationX = viewport.toViewX(input.cursorX) - cursor.width * 0.5f
        cursor.translationY = viewport.toViewY(input.cursorY) - cursor.height * 0.5f
    }

    /** Puts the cursor under a touch point. */
    fun pointAt(touchX: Float, touchY: Float) {
        input.cursorX = viewport.toNormX(touchX)
        input.cursorY = viewport.toNormY(touchY)
        updateCursor()
    }

    private fun setFrameSize(width: Int, height: Int) {
        if (width == frameWidth && height == frameHeight) return
        frameWidth = width
        frameHeight = height
        layout()
    }

    private companion object {
        const val OPAQUE = 255
    }
}
