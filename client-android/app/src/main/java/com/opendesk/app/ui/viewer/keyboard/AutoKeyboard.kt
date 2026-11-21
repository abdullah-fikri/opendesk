package com.opendesk.app.ui.viewer.keyboard

/**
 * When the keyboard raises and lowers itself on the host's `text-focus` events: it comes up only for a field the
 * user just tapped (focus moving by itself, a dialog opening, must not cover the screen), and goes down after a
 * short delay (focus hopping between two fields passes through "nothing focused"), and only if it came up by
 * itself (a keyboard the user opened stays until they close it).
 */
class AutoKeyboard(private val tapWindowMs: Long = TAP_WINDOW_MS) {
    enum class Decision { SHOW, HIDE_LATER, CANCEL_HIDE, NONE }

    private var lastTapMs = Long.MIN_VALUE / 2

    /** The keyboard is up because of a `text-focus` event. */
    var autoShown = false
        private set

    /** The host's last word: keyboard focus is in a text field. */
    var focused = false
        private set

    fun onTap(nowMs: Long) {
        lastTapMs = nowMs
    }

    /**
     * A tap while the host's focus already sat in a text field sends no `text-focus` (nothing changed), so the
     * caller checks again a moment after the tap: still focused and no keyboard up means the user tapped back into
     * that field (or into the same editor), and the keyboard comes up.
     */
    fun showAfterTap(visible: Boolean): Boolean {
        if (!focused || visible) return false
        autoShown = true
        return true
    }

    /** A `text-focus` event; `visible`: a keyboard (glass or system) is up now. */
    fun onFocus(editable: Boolean, visible: Boolean, nowMs: Long): Decision {
        focused = editable
        return decide(editable, visible, nowMs)
    }

    private fun decide(editable: Boolean, visible: Boolean, nowMs: Long): Decision = when {
        editable && visible -> Decision.CANCEL_HIDE
        editable && nowMs - lastTapMs <= tapWindowMs -> {
            autoShown = true
            Decision.SHOW
        }
        editable -> Decision.NONE
        visible && autoShown -> Decision.HIDE_LATER
        else -> Decision.NONE
    }

    /** The user opened or closed a keyboard by hand: it is theirs now. */
    fun onManual() {
        autoShown = false
    }

    /** The keyboard went down (by the delayed hide or otherwise). */
    fun onHidden() {
        autoShown = false
    }

    companion object {
        /** A click reaches the host ~300 ms after the finger lifts (double-tap wait), then poll, debounce and the
         *  network: a field the user tapped reports back well within this. */
        const val TAP_WINDOW_MS = 3000L

        /** How long after a tap the host's unchanged focus is trusted (its `editable: false` would come first). */
        const val RETAP_CHECK_MS = 1100L
        const val HIDE_DELAY_MS = 400L
    }
}
