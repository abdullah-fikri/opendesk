package com.opendesk.app.ui.viewer

import android.annotation.SuppressLint
import android.os.SystemClock
import android.view.Choreographer
import android.view.GestureDetector
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.widget.Toast
import com.opendesk.app.R
import com.opendesk.app.domain.WheelScroll
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
 * Scrolling follows the finger ([WheelScroll]: the remote page moves about as far as the finger) and a flick keeps
 * coasting until friction stops it or a new touch catches it.
 *
 * `setLeftHeld` releases a held button (a tap while Hold is on).
 */
class TouchController(
    private val screen: View,
    private val state: ViewerState,
    private val input: RemoteInput,
    private val renderer: ScreenRenderer,
    private val setLeftHeld: (Boolean) -> Unit,
    /** A swipe down that started at the top edge (shows the session pill). */
    private val onTopSwipe: () -> Unit = {},
) {
    /** What the current two-finger gesture turned out to be. */
    private enum class TwoFinger { UNDECIDED, SCROLL, ZOOM }

    private val viewport get() = renderer.viewport

    private var lastTouchX = 0f
    private var lastTouchY = 0f

    /** Where the current touch started, when that was at the top edge (a possible swipe down to the pill). */
    private var topSwipeFromY = Float.NaN
    private val topZonePx = TOP_ZONE_DP * screen.resources.displayMetrics.density
    private val topSwipePx = TOP_SWIPE_DP * screen.resources.displayMetrics.density

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

    private val wheel = WheelScroll()

    /** Finger speed while scrolling, px/s on the phone (smoothed), and when it was last measured. */
    private var scrollVelocity = 0f
    private var lastScrollMs = 0L
    private var flingVelocity = 0f
    private var flingFrameNs = 0L
    private val flingFrame = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            val dtMs = ((frameTimeNanos - flingFrameNs) / 1_000_000f).coerceIn(1f, 50f)
            flingFrameNs = frameTimeNanos
            scrollBy(flingVelocity * dtMs / 1000f)
            flingVelocity = WheelScroll.decay(flingVelocity, dtMs)
            if (WheelScroll.coasting(flingVelocity)) Choreographer.getInstance().postFrameCallback(this)
        }
    }

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
                    stopFling()
                    if (state.scrollMode) startScroll()
                    multiTouch = false
                    lastTouchX = event.x
                    lastTouchY = event.y
                    topSwipeFromY = if (event.y < topZonePx) event.y else Float.NaN
                    if (!state.trackpadMode) {
                        renderer.pointAt(event.x, event.y)
                        input.move(force = true)
                    }
                }
                MotionEvent.ACTION_POINTER_DOWN -> {
                    multiTouch = true
                    if (event.pointerCount == 2) startTwoFinger(event)
                }
                MotionEvent.ACTION_MOVE -> {
                    if (!topSwipeFromY.isNaN() && event.y - topSwipeFromY > topSwipePx) {
                        topSwipeFromY = Float.NaN
                        onTopSwipe()
                    }
                    if (event.pointerCount >= 2) onTwoFingerMove(event) else if (!multiTouch) onMove(event)
                }
                MotionEvent.ACTION_POINTER_UP -> if (event.pointerCount == 2) {
                    // A quick two-finger tap is a right click; lifting from a two-finger scroll lets it coast.
                    if (twoFingerMode == TwoFinger.UNDECIDED &&
                        SystemClock.uptimeMillis() - twoFingerDownTime < TWO_FINGER_TAP_MS
                    ) {
                        screen.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                        input.mouse("click", BUTTON_RIGHT)
                    } else if (twoFingerMode == TwoFinger.SCROLL) {
                        startFling()
                    }
                }
                MotionEvent.ACTION_UP -> if (!multiTouch) {
                    if (state.scrollMode) startFling() else input.move(force = true)
                }
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
        startScroll()
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
                track(focusY - lastScrollY)
                lastScrollY = focusY
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
            track(event.y - lastTouchY)
            lastTouchY = event.y
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
            if (viewport.overflows) {
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

    // ---- Scrolling ----

    private fun startScroll() {
        wheel.reset()
        scrollVelocity = 0f
        lastScrollMs = SystemClock.uptimeMillis()
    }

    /** The scrolling finger moved `dy` phone pixels: scroll by as much and update the finger speed. */
    private fun track(dy: Float) {
        if (dy == 0f) return
        val now = SystemClock.uptimeMillis()
        val dt = (now - lastScrollMs).coerceAtLeast(1L)
        val instant = dy * 1000f / dt
        scrollVelocity = if (dt > VELOCITY_STALE_MS) instant else 0.6f * instant + 0.4f * scrollVelocity
        lastScrollMs = now
        scrollBy(dy)
    }

    /** Scrolls by `dy` phone pixels (positive = finger down = page up). */
    private fun scrollBy(dy: Float) {
        val delta = wheel.add(dy / viewport.scale.coerceAtLeast(MIN_SCALE))
        if (delta != 0) input.wheel(delta)
    }

    /** The finger left: coast with its speed, unless it had stopped before lifting. */
    private fun startFling() {
        val fresh = SystemClock.uptimeMillis() - lastScrollMs <= VELOCITY_STALE_MS
        flingVelocity = if (fresh) scrollVelocity else 0f
        if (!WheelScroll.coasting(flingVelocity)) return
        flingFrameNs = System.nanoTime()
        Choreographer.getInstance().postFrameCallback(flingFrame)
    }

    private fun stopFling() {
        flingVelocity = 0f
        Choreographer.getInstance().removeFrameCallback(flingFrame)
    }

    private companion object {
        const val TRACKPAD_SENSITIVITY = 1.35f

        /** A touch starting this close to the top that moves this far down shows the session pill. */
        const val TOP_ZONE_DP = 36f
        const val TOP_SWIPE_DP = 48f

        /** Finger travel that decides a two-finger gesture is a scroll. */
        const val SCROLL_THRESHOLD_PX = 18f

        /** A finger that has not moved for this long is resting: its speed no longer counts for a flick. */
        const val VELOCITY_STALE_MS = 80L

        /** Floor for the frame scale, so a not-yet-laid-out frame cannot turn a scroll into a flood. */
        const val MIN_SCALE = 0.05f
        const val ZOOM_SLOP_PX = 24f
        const val TWO_FINGER_TAP_MS = 300L
        const val CURSOR_MARGIN_PX = 48f
    }
}
