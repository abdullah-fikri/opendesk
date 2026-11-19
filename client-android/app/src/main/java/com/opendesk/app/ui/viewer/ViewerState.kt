package com.opendesk.app.ui.viewer

/** What the viewer's controls switch; read by the controls overlay and the touch handling. */
class ViewerState {
    /** Trackpad: one finger moves a virtual cursor. Touch: the cursor jumps to the finger. */
    var trackpadMode = true

    /** One-finger drags turn the wheel instead of moving the pointer (the Scroll control). */
    var scrollMode = false

    /** The left button is held down (the Hold control) until tapped again. */
    var leftHeld = false

    var controlsOpen = false
    var keyboardVisible = false
}
