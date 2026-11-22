package com.opendesk.app.ui.viewer

import android.animation.Animator
import android.animation.AnimatorListenerAdapter
import android.animation.ValueAnimator
import android.view.View
import android.view.animation.DecelerateInterpolator
import com.opendesk.app.domain.FocusPan

/**
 * Keeps the host's focused text field visible above the keyboard: while a keyboard is up and the host told where
 * the field is (`text-focus`), the remote screen glides (zooming in on a tiny field) so the field sits just above
 * the keyboard's top edge ([FocusPan.target]). When the keyboard goes down the screen glides back to where it was,
 * unless the user panned or zoomed in between (then it stays, within the normal limits).
 *
 * `screen` is the view whose coordinates the viewport uses (the glass keyboard shares them). Main thread.
 */
class FocusPanner(private val renderer: ScreenRenderer, private val screen: View) {
    private val dp = screen.resources.displayMetrics.density
    private val viewport get() = renderer.viewport

    /** The focused field, or null when focus is not in a text field (or the host gave no rect). */
    private var field: FocusPan.Field? = null

    /** Top edge of the keyboard in screen coordinates; null: no keyboard. */
    private var keyboardTop: Float? = null

    /** Where the screen was before the first automatic move, and where the last one went. */
    private var saved: FocusPan.State? = null
    private var applied: FocusPan.State? = null
    private var animator: ValueAnimator? = null

    init {
        // The phone's keyboard shrinks the screen view: once it is laid out again, place the field again.
        screen.addOnLayoutChangeListener { _, _, top, _, bottom, _, oldTop, _, oldBottom ->
            if (bottom - top != oldBottom - oldTop && keyboardTop != null) screen.post { update() }
        }
    }

    /** The host's focus moved: into `field`, or out of text fields (null). */
    fun onField(field: FocusPan.Field?) {
        this.field = field
        if (field != null) update()
    }

    /**
     * A keyboard came up, moved or went down. `top`: its top edge in screen coordinates; for the phone's keyboard,
     * which shrinks the screen view, the view's height.
     */
    fun onKeyboard(top: Float?) {
        val wasUp = keyboardTop != null
        keyboardTop = top
        when {
            top != null -> update()
            wasUp -> restore()
        }
    }

    /** The user touched the screen: an automatic move in progress stops where it is. */
    fun onUserTouch() {
        if (animator?.isRunning != true) return
        animator?.cancel()
        // Now theirs: no glide back.
        applied = null
        if (keyboardTop == null && viewport.bottomInset > 0f) {
            // Stopped on the way back: no keyboard any more, so no extra room either.
            viewport.bottomInset = 0f
            renderer.apply()
        }
    }

    fun release() {
        animator?.cancel()
        animator = null
    }

    private fun update() {
        val top = keyboardTop ?: return
        val f = field ?: return
        if (screen.width == 0 || screen.height == 0) return
        // The keyboard floats over the picture: the frame never moves off the screen (no black band under it) and
        // is never zoomed by itself. Only a view the user zoomed or panned scrolls, within its usual limits, to
        // bring the field above the keyboard.
        val from = current()
        if (saved == null) saved = from
        val target = FocusPan.target(viewport, f, top, MARGIN_DP * dp, minFieldHeight = 0f, extraRoom = false)
        viewport.bottomInset = 0f
        applied = target
        animateTo(target, then = null)
    }

    private fun restore() {
        val back = saved
        val untouched = applied?.let { current().near(it) } == true
        saved = null
        applied = null
        animator?.cancel()
        if (back != null && untouched) {
            // The keyboard's inset stays until the glide ends so the steps are not clamped on the way.
            animateTo(back) { clearInset() }
        } else {
            clearInset()
        }
    }

    /** Drops the keyboard's extra pan room, gliding back within the normal limits if the frame is in it. */
    private fun clearInset() {
        if (viewport.bottomInset == 0f) return
        val inLimits = FocusPan.state(viewport.copy().also { it.bottomInset = 0f })
        animateTo(inLimits) {
            viewport.bottomInset = 0f
            renderer.apply()
        }
    }

    private fun current() = FocusPan.state(viewport)

    private fun animateTo(target: FocusPan.State, then: (() -> Unit)?) {
        animator?.cancel()
        val from = current()
        if (from.near(target, 0.5f)) {
            viewport.set(target.zoom, target.panX, target.panY)
            renderer.apply()
            then?.invoke()
            return
        }
        animator = ValueAnimator.ofFloat(0f, 1f).apply {
            duration = DURATION_MS
            interpolator = DecelerateInterpolator(1.6f)
            addUpdateListener {
                val s = FocusPan.lerp(from, target, it.animatedValue as Float)
                viewport.set(s.zoom, s.panX, s.panY)
                renderer.apply()
            }
            if (then != null) {
                addListener(object : AnimatorListenerAdapter() {
                    private var cancelled = false
                    override fun onAnimationCancel(animation: Animator) {
                        cancelled = true
                    }

                    override fun onAnimationEnd(animation: Animator) {
                        if (!cancelled) then()
                    }
                })
            }
            start()
        }
    }

    private companion object {
        /** Gap between the field and the keyboard's top edge. */
        const val MARGIN_DP = 24f
        const val DURATION_MS = 200L
    }
}
