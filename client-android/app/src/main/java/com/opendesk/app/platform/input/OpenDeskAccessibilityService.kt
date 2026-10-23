package com.opendesk.app.platform.input

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Path
import android.view.accessibility.AccessibilityEvent

/** Injects taps and global actions (Back, Home) for a controlling PC. Enabled by the user in system settings. */
class OpenDeskAccessibilityService : AccessibilityService() {

    companion object {
        var instance: OpenDeskAccessibilityService? = null
            private set

        val isRunning: Boolean
            get() = instance != null
    }

    override fun onServiceConnected() {
        super.onServiceConnected()
        instance = this
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}

    override fun onInterrupt() {}

    override fun onDestroy() {
        super.onDestroy()
        instance = null
    }

    /** Taps the screen at (`x`, `y`) in pixels. */
    fun dispatchClick(x: Float, y: Float) {
        val path = Path().apply {
            moveTo(x, y)
        }
        val gesture = GestureDescription.Builder()
            .addStroke(GestureDescription.StrokeDescription(path, 0, 50))
            .build()
        dispatchGesture(gesture, null, null)
    }

    /** Presses Back. */
    fun triggerBack() {
        performGlobalAction(GLOBAL_ACTION_BACK)
    }

    /** Presses Home. */
    fun triggerHome() {
        performGlobalAction(GLOBAL_ACTION_HOME)
    }
}
