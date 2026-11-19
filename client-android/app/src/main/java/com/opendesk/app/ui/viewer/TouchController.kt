package com.opendesk.app.ui.viewer

import android.annotation.SuppressLint
import android.os.SystemClock
import android.view.GestureDetector
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.widget.Toast
import com.opendesk.app.R
import com.opendesk.app.ui.viewer.RemoteInput.Companion.BUTTON_LEFT
import com.opendesk.app.ui.viewer.RemoteInput.Companion.BUTTON_RIGHT
import kotlin.math.abs
import kotlin.math.hypot

/**
 * Turns touches on the remote screen into input. Tap = left click, double tap = double click, long press or
 * two-finger tap = right click, two-finger vertical drag = scroll wheel, pinch = zoom (moving the fingers while
 * pinching pans). In trackpad mode one finger moves the virtual cursor and a zoomed view follows it; in touch mode
 * the cursor jumps to the finger.
 *
 * `setLeftHeld` releases a held button (a tap while Hold is on).
 */
class TouchController(
    private val screen: View,
    private val state: ViewerState,
    private val input: RemoteInput,
    private val renderer: ScreenRenderer,
    private val setLeftHeld: (Boolean) -> Unit,
) {
    /** What the current two-finger gesture turned out to be. */
    private enum class TwoFinger { UNDECIDED, SCROLL, ZOOM }

    private val viewport get() = renderer.viewport

    private var lastTouchX = 0f
    private var lastTouchY = 0f

    /** More than one finger touched during the current gesture: ignore the remaining finger's moves. */
    private var multiTouch = false
    private var twoFingerMode = TwoFinger.UNDECIDED
    private var twoFingerDownTime = 0L
    private var twoFingerStartSpan = 0f
    private var twoFingerStartY = 0f
    private var lastSpan = 0f
    private var lastFocusX = 0f
    private var lastFocusY = 0f
    private var lastScrollY = 0f

    private val gestures = GestureDetector(screen.context, object : GestureDetector.SimpleOnGestureListener() {
        override fun onSingleTapConfirmed(e: MotionEvent): Boolean {
            if (multiTouch) return true
            if (!state.trackpadMode) renderer.pointAt(e.x, e.y)
            if (state.leftHeld) setLeftHeld(false) else input.mouse("click", BUTTON_LEFT)
            return true
        }

        override fun onDoubleTap(e: MotionEvent): Boolean {
            if (multiTouch) return true
            if (!state.trackpadMode) renderer.pointAt(e.x, e.y)
            screen.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            input.mouse("dblclick", BUTTON_LEFT)
            return true
        }

        override fun onLongPress(e: MotionEvent) {
            if (multiTouch) return
            if (!state.trackpadMode) renderer.pointAt(e.x, e.y)
            screen.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            input.mouse("click", BUTTON_RIGHT)
            Toast.makeText(screen.context, R.string.right_click, Toast.LENGTH_SHORT).show()
        }
    })

    @SuppressLint("ClickableViewAccessibility") // Touches are forwarded to the remote machine, not clicks here.
    fun attach() {
        screen.setOnTouchListener { _, event ->
            gestures.onTouchEvent(event)
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    multiTouch = false
                    lastTouchX = event.x
                    lastTouchY = event.y
                    if (!state.trackpadMode) {
                        renderer.pointAt(event.x, event.y)
                        input.move(force = true)
                    }
                }
                MotionEvent.ACTION_POINTER_DOWN -> {
                    multiTouch = true
                    if (event.pointerCount == 2) startTwoFinger(event)
                }
                MotionEvent.ACTION_MOVE -> if (event.pointerCount >= 2) onTwoFingerMove(event) else if (!multiTouch) onMove(event)
                // A quick two-finger tap is a right click.
                MotionEvent.ACTION_POINTER_UP -> if (event.pointerCount == 2 && twoFingerMode == TwoFinger.UNDECIDED &&
                    SystemClock.uptimeMillis() - twoFingerDownTime < TWO_FINGER_TAP_MS
                ) {
                    screen.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                    input.mouse("click", BUTTON_RIGHT)
                }
                MotionEvent.ACTION_UP -> if (!multiTouch) input.move(force = true)
            }
            true
        }
    }

    private fun startTwoFinger(event: MotionEvent) {
        twoFingerMode = TwoFinger.UNDECIDED
        twoFingerDownTime = SystemClock.uptimeMillis()
        twoFingerStartSpan = span(event)
        lastSpan = twoFingerStartSpan
        lastFocusX = (event.getX(0) + event.getX(1)) * 0.5f
        lastFocusY = (event.getY(0) + event.getY(1)) * 0.5f
        twoFingerStartY = lastFocusY
        lastScrollY = lastFocusY
    }

    /** Two fingers: the first clear movement decides between scrolling (parallel drag) and zooming (pinch). */
    private fun onTwoFingerMove(event: MotionEvent) {
        val span = span(event)
        val focusX = (event.getX(0) + event.getX(1)) * 0.5f
        val focusY = (event.getY(0) + event.getY(1)) * 0.5f
        if (twoFingerMode == TwoFinger.UNDECIDED) {
            twoFingerMode = when {
                abs(span - twoFingerStartSpan) > ZOOM_SLOP_PX -> TwoFinger.ZOOM
                abs(focusY - twoFingerStartY) > SCROLL_THRESHOLD_PX -> TwoFinger.SCROLL
                else -> TwoFinger.UNDECIDED
            }
        }
        when (twoFingerMode) {
            TwoFinger.ZOOM -> {
                if (lastSpan > 0f) viewport.zoomBy(span / lastSpan, focusX, focusY)
                viewport.panBy(focusX - lastFocusX, focusY - lastFocusY)
                renderer.apply()
            }
            TwoFinger.SCROLL -> {
                val dy = focusY - lastScrollY
                if (abs(dy) > SCROLL_THRESHOLD_PX) {
                    input.wheel(if (dy > 0) SCROLL_STEP else -SCROLL_STEP)
                    lastScrollY = focusY
                }
            }
            TwoFinger.UNDECIDED -> Unit
        }
        lastSpan = span
        lastFocusX = focusX
        lastFocusY = focusY
    }

    private fun span(event: MotionEvent): Float = hypot(event.getX(0) - event.getX(1), event.getY(0) - event.getY(1))

    private fun onMove(event: MotionEvent) {
        if (state.scrollMode) {
            val dy = event.y - lastTouchY
            if (abs(dy) > SCROLL_THRESHOLD_PX) {
                input.wheel(if (dy > 0) SCROLL_STEP else -SCROLL_STEP)
                lastTouchY = event.y
            }
            return
        }
        if (state.trackpadMode) {
            // Relative to the drawn frame: the same on-screen speed at any zoom, so zooming in gives finer control.
            input.cursorX = (input.cursorX + (event.x - lastTouchX) / viewport.drawnWidth.coerceAtLeast(1f) * TRACKPAD_SENSITIVITY)
                .coerceIn(0f, 1f)
            input.cursorY = (input.cursorY + (event.y - lastTouchY) / viewport.drawnHeight.coerceAtLeast(1f) * TRACKPAD_SENSITIVITY)
                .coerceIn(0f, 1f)
            lastTouchX = event.x
            lastTouchY = event.y
            if (viewport.zoom > 1f) {
                viewport.ensureVisible(input.cursorX, input.cursorY, CURSOR_MARGIN_PX)
                renderer.apply()
            } else {
                renderer.updateCursor()
            }
        } else {
            renderer.pointAt(event.x, event.y)
        }
        input.move(force = false)
    }

    private companion object {
        const val TRACKPAD_SENSITIVITY = 1.35f
        const val SCROLL_STEP = 120
        const val SCROLL_THRESHOLD_PX = 18f
        const val ZOOM_SLOP_PX = 24f
        const val TWO_FINGER_TAP_MS = 300L
        const val CURSOR_MARGIN_PX = 48f
    }
}
