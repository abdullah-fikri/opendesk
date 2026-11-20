package com.opendesk.app.ui.viewer

import android.graphics.Bitmap
import android.graphics.Matrix
import android.view.View
import android.widget.ImageView
import com.opendesk.app.domain.Viewport

/** Draws the remote frame with the current fit, zoom and pan, and places the virtual cursor over it. */
class ScreenRenderer(private val screen: ImageView, private val cursor: View, private val input: RemoteInput) {
    val viewport = Viewport()
    private val matrix = Matrix()

    init {
        cursor.visibility = View.VISIBLE
        screen.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> layout() }
    }

    /** Shows a decoded frame; a frame of a new size recomputes the fit. */
    fun show(bitmap: Bitmap) {
        val sizeChanged = bitmap.width != viewport.imageWidth || bitmap.height != viewport.imageHeight
        screen.setImageBitmap(bitmap)
        if (sizeChanged) layout()
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
        val frame = screen.drawable
        viewport.resize(screen.width, screen.height, frame?.intrinsicWidth ?: 0, frame?.intrinsicHeight ?: 0)
        apply()
    }

    /** Applies the viewport to the image after a zoom or pan. */
    fun apply() {
        matrix.setScale(viewport.scale, viewport.scale)
        matrix.postTranslate(viewport.left, viewport.top)
        screen.imageMatrix = matrix
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
}
