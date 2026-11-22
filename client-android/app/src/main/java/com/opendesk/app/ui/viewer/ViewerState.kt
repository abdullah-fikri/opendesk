package com.opendesk.app.ui.viewer

/** What the viewer's controls switch; read by the controls overlay and the touch handling. */
class ViewerState {
    /** Trackpad: one finger moves a virtual cursor. Touch: the cursor jumps to the finger. */
    var trackpadMode = true

    /** One-finger drags turn the wheel instead of moving the pointer (the Scroll control). */
    var scrollMode = false

    /** The left button is held down (the Hold control) until tapped again. */
    var leftHeld = false

    /** The frame covers the whole screen (edges reached by panning) instead of fitting inside it. */
    var fill = false

    var controlsOpen = false
    /** The phone's own keyboard (IME) is up. */
    var keyboardVisible = false

    /** The mini glass keyboard is up. */
    var glassKeyboardVisible = false
}
