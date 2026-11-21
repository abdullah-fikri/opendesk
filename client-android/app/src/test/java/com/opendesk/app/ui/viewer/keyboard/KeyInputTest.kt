package com.opendesk.app.ui.viewer.keyboard

import com.opendesk.app.domain.WindowsKeys
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Kind
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Mod
import com.opendesk.app.ui.viewer.keyboard.KeyInput.Output
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Page
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class KeyInputTest {
    private fun key(page: Page, label: String) = KeyboardLayouts.rows(page).flatten().first { it.label == label }

    @Test
    fun shiftOnceThenCapsLockOnDoubleTap() {
        val shift = KeyInput.Shift()
        shift.tap(1000)
        assertEquals(KeyInput.Shift.State.ONCE, shift.state)
        shift.consume()
        assertEquals(KeyInput.Shift.State.OFF, shift.state)
        shift.tap(2000)
        shift.tap(2200)
        assertEquals(KeyInput.Shift.State.LOCKED, shift.state)
        shift.consume()
        assertEquals("caps lock survives typing", KeyInput.Shift.State.LOCKED, shift.state)
        shift.tap(5000)
        assertEquals(KeyInput.Shift.State.OFF, shift.state)
        shift.tap(6000)
        shift.tap(7000)
        assertEquals("slow second tap turns it off", KeyInput.Shift.State.OFF, shift.state)
    }

    @Test
    fun modifiersAreStickyForOneKeyUnlessLocked() {
        val mods = KeyInput.Modifiers()
        assertFalse(mods.tap(Mod.CTRL, 0))
        assertTrue(mods.isOn(Mod.CTRL))
        mods.consume()
        assertFalse(mods.isOn(Mod.CTRL))
        mods.tap(Mod.ALT, 1000)
        mods.tap(Mod.ALT, 1100)
        assertTrue(mods.isLocked(Mod.ALT))
        mods.consume()
        assertTrue(mods.isOn(Mod.ALT))
        mods.tap(Mod.ALT, 3000)
        assertFalse(mods.isOn(Mod.ALT))
    }

    @Test
    fun secondWinTapIsTheStartMenu() {
        val mods = KeyInput.Modifiers()
        assertFalse(mods.tap(Mod.WIN, 0))
        assertTrue(mods.isOn(Mod.WIN))
        assertTrue(mods.tap(Mod.WIN, 100))
        assertFalse(mods.isOn(Mod.WIN))
    }

    @Test
    fun lettersTypeAsTextAndBecomeCombosWithModifiers() {
        val mods = KeyInput.Modifiers()
        val c = key(Page.LETTERS, "c")
        assertEquals(Output.Text("c"), KeyInput.output(c, shift = false, mods = mods))
        assertEquals(Output.Text("C"), KeyInput.output(c, shift = true, mods = mods))
        mods.tap(Mod.CTRL, 0)
        assertEquals(Output.Key('C'.code, ctrl = true), KeyInput.output(c, shift = false, mods = mods))
        mods.reset()
        mods.tap(Mod.WIN, 0)
        assertEquals(Output.Key('R'.code, win = true), KeyInput.output(key(Page.LETTERS, "r"), false, mods))
        // A symbol has no key to combine: it is typed.
        mods.reset()
        mods.tap(Mod.CTRL, 0)
        assertEquals(Output.Text("@"), KeyInput.output(key(Page.NUMBERS, "@"), false, mods))
    }

    @Test
    fun specialKeysSendVirtualKeys() {
        val mods = KeyInput.Modifiers()
        assertEquals(Output.Key(WindowsKeys.VK_ESCAPE), KeyInput.output(key(Page.LETTERS, "Esc"), false, mods))
        assertEquals(Output.Key(WindowsKeys.VK_LEFT, shift = true), KeyInput.output(key(Page.LETTERS, "←"), true, mods))
        val backspace = KeyboardLayouts.rows(Page.LETTERS).flatten().first { it.kind == Kind.BACKSPACE }
        assertEquals(Output.Key(WindowsKeys.VK_BACK), KeyInput.output(backspace, false, mods))
        val enter = KeyboardLayouts.rows(Page.LETTERS).flatten().first { it.kind == Kind.ENTER }
        assertEquals(Output.Key(WindowsKeys.VK_RETURN), KeyInput.output(enter, false, mods))
        assertNull(KeyInput.output(key(Page.LETTERS, "Ctrl"), false, mods))
        assertNull(KeyInput.output(key(Page.LETTERS, "123"), false, mods))
    }

    @Test
    fun backspaceRepeatsAfterAPause() {
        assertEquals(400L, KeyInput.repeatDelayMs(0))
        assertEquals(50L, KeyInput.repeatDelayMs(1))
        assertEquals(50L, KeyInput.repeatDelayMs(20))
    }
}
