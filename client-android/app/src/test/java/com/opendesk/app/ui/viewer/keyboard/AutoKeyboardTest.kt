package com.opendesk.app.ui.viewer.keyboard

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AutoKeyboardTest {
    @Test
    fun keyboardComesUpOnlyForAFieldJustTapped() {
        val auto = AutoKeyboard()
        assertEquals(AutoKeyboard.Decision.NONE, auto.onFocus(editable = true, visible = false, nowMs = 10_000))
        auto.onTap(20_000)
        assertEquals(AutoKeyboard.Decision.SHOW, auto.onFocus(true, false, 20_300))
        assertTrue(auto.autoShown)
        // Focus hops to another field through "nothing": hide is delayed, then cancelled.
        assertEquals(AutoKeyboard.Decision.HIDE_LATER, auto.onFocus(false, true, 21_000))
        assertEquals(AutoKeyboard.Decision.CANCEL_HIDE, auto.onFocus(true, true, 21_100))
        auto.onTap(30_000)
        assertEquals("too late after the tap", AutoKeyboard.Decision.NONE, auto.onFocus(true, false, 33_100))
    }

    @Test
    fun tappingBackIntoAFocusedFieldRaisesTheKeyboard() {
        val auto = AutoKeyboard()
        // Focus already sat in a field (no tap): nothing comes up by itself.
        assertEquals(AutoKeyboard.Decision.NONE, auto.onFocus(editable = true, visible = false, nowMs = 1000))
        assertTrue(auto.focused)
        // A later tap changes nothing on the host; the check after it raises the keyboard.
        assertTrue(auto.showAfterTap(visible = false))
        assertTrue(auto.autoShown)
        assertFalse("already up", auto.showAfterTap(visible = true))
        // Focus left the field: a tap no longer raises it.
        auto.onFocus(editable = false, visible = false, nowMs = 2000)
        assertFalse(auto.showAfterTap(visible = false))
    }

    @Test
    fun aKeyboardOpenedByHandStays() {
        val auto = AutoKeyboard()
        auto.onManual()
        assertEquals(AutoKeyboard.Decision.NONE, auto.onFocus(editable = false, visible = true, nowMs = 1000))
        auto.onTap(2000)
        assertEquals(AutoKeyboard.Decision.CANCEL_HIDE, auto.onFocus(true, true, 2100))
        assertEquals("still the user's", AutoKeyboard.Decision.NONE, auto.onFocus(false, true, 2500))
    }
}
