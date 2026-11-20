package com.opendesk.app.ui.viewer

import android.os.SystemClock
import com.opendesk.app.data.dto.InputEvent
import com.opendesk.app.domain.WindowsKeys
import com.opendesk.app.session.RelayConnection

/** Sends mouse, wheel and keyboard input to the computer, at the virtual cursor's position. */
class RemoteInput {
    /** Virtual cursor in normalized coordinates [0..1]. */
    var cursorX = 0.5f
    var cursorY = 0.5f

    private var lastMoveSentMs = 0L

    /** Moves the pointer to the cursor; unforced moves are throttled to one every [MOVE_THROTTLE_MS]. */
    fun move(force: Boolean) {
        val now = SystemClock.uptimeMillis()
        if (!force && now - lastMoveSentMs < MOVE_THROTTLE_MS) return
        lastMoveSentMs = now
        send(InputEvent(action = "mousemove", normX = cursorX, normY = cursorY))
    }

    /** `action`: click, dblclick, mousedown or mouseup. */
    fun mouse(action: String, button: Int) = send(InputEvent(action = action, normX = cursorX, normY = cursorY, button = button))

    fun wheel(deltaY: Int) = send(InputEvent(action = "wheel", normX = cursorX, normY = cursorY, deltaY = deltaY))

    /** Presses and releases a Win32 virtual key. */
    fun key(vk: Int) {
        send(InputEvent(action = "keydown", keyCode = vk))
        send(InputEvent(action = "keyup", keyCode = vk))
    }

    /** Typed text; a soft keyboard may commit a newline as text, and the computer gets a real Enter key for it. */
    fun text(text: String) {
        text.split('\n').forEachIndexed { i, part ->
            if (i > 0) key(WindowsKeys.VK_RETURN)
            if (part.isNotEmpty()) send(InputEvent(action = "text", text = part))
        }
    }

    fun winKey() = shortcut("win")

    /** A host shortcut: `win`, `alt-tab`, `taskmgr`, `explorer` or `desktop`. */
    fun shortcut(name: String) = send(InputEvent(action = "shortcut", name = name))

    fun switchMonitor() = send(InputEvent(action = "switch-monitor"))

    fun requestKeyFrame() = send(InputEvent(action = "video-keyframe"))

    private fun send(event: InputEvent) = RelayConnection.sendInput(event)

    companion object {
        const val BUTTON_LEFT = 0
        const val BUTTON_RIGHT = 2
        private const val MOVE_THROTTLE_MS = 15L
    }
}
