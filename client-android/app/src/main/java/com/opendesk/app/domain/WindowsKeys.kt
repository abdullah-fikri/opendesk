package com.opendesk.app.domain

/**
 * Maps Android key codes (`android.view.KeyEvent.KEYCODE_*`) of non-printing keys to Win32 virtual-key codes for
 * `keydown`/`keyup` events. Printable characters are sent as `text` instead, so the host's keyboard layout does
 * not matter. The numeric values are the platform's constants, repeated here so this stays plain Kotlin.
 */
object WindowsKeys {
    const val VK_BACK = 0x08
    const val VK_TAB = 0x09
    const val VK_RETURN = 0x0D
    const val VK_ESCAPE = 0x1B
    const val VK_PRIOR = 0x21
    const val VK_NEXT = 0x22
    const val VK_END = 0x23
    const val VK_HOME = 0x24
    const val VK_LEFT = 0x25
    const val VK_UP = 0x26
    const val VK_RIGHT = 0x27
    const val VK_DOWN = 0x28
    const val VK_DELETE = 0x2E

    private val androidToVk = mapOf(
        67 to VK_BACK, // KEYCODE_DEL
        61 to VK_TAB, // KEYCODE_TAB
        66 to VK_RETURN, // KEYCODE_ENTER
        160 to VK_RETURN, // KEYCODE_NUMPAD_ENTER
        111 to VK_ESCAPE, // KEYCODE_ESCAPE
        92 to VK_PRIOR, // KEYCODE_PAGE_UP
        93 to VK_NEXT, // KEYCODE_PAGE_DOWN
        123 to VK_END, // KEYCODE_MOVE_END
        122 to VK_HOME, // KEYCODE_MOVE_HOME
        21 to VK_LEFT, // KEYCODE_DPAD_LEFT
        19 to VK_UP, // KEYCODE_DPAD_UP
        22 to VK_RIGHT, // KEYCODE_DPAD_RIGHT
        20 to VK_DOWN, // KEYCODE_DPAD_DOWN
        112 to VK_DELETE, // KEYCODE_FORWARD_DEL
    )

    /** Win32 virtual key for an Android key code, or null when the key is printable or has no equivalent. */
    fun fromAndroid(keyCode: Int): Int? = androidToVk[keyCode]

    /**
     * What a soft keyboard's composing-text change means for the host: how many characters to erase with
     * Backspace, then which text to type. Only the part after the common prefix is resent.
     */
    fun diff(before: String, after: String): Pair<Int, String> {
        // commonPrefixWith never ends inside a surrogate pair, so emoji are erased and retyped whole.
        val common = before.commonPrefixWith(after).length
        val removed = before.substring(common)
        return removed.codePointCount(0, removed.length) to after.substring(common)
    }
}
