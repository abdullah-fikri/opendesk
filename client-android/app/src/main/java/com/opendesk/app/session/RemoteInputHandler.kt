package com.opendesk.app.session

import android.content.Context
import com.opendesk.app.data.dto.RemoteEvent
import com.opendesk.app.platform.input.OpenDeskAccessibilityService

/**
 * Applies a controlling PC's input to this phone through the accessibility service: clicks become taps,
 * Escape is Back and the Windows key is Home. Other events have no phone equivalent and are ignored.
 */
class RemoteInputHandler(private val context: Context) {

    /** Applies one event from the controlling PC; does nothing without the accessibility service. */
    fun apply(event: RemoteEvent) {
        val service = OpenDeskAccessibilityService.instance ?: return
        when (event.action) {
            "click", "mouseup" -> {
                val x = event.normX ?: return
                val y = event.normY ?: return
                val metrics = context.resources.displayMetrics
                service.dispatchClick(x * metrics.widthPixels, y * metrics.heightPixels)
            }
            "keydown" -> when (event.keyCode) {
                VK_ESCAPE -> service.triggerBack()
                VK_LWIN -> service.triggerHome()
            }
        }
    }

    private companion object {
        const val VK_ESCAPE = 27
        const val VK_LWIN = 91
    }
}
