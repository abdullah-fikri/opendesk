package com.opendesk.app.ui.viewer.keyboard

import com.opendesk.app.domain.WindowsKeys
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Key
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Kind
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Mod

/**
 * What the glass keyboard's keys do (pure, unit-tested): Shift and the sticky modifiers, key repeat timing, and
 * what a key press sends to the host.
 */
object KeyInput {
    /** Shift: one tap = the next letter only, a double tap = caps lock, a tap while on = off. */
    class Shift(private val doubleTapMs: Long = DOUBLE_TAP_MS) {
        enum class State { OFF, ONCE, LOCKED }

        var state = State.OFF
            private set
        private var lastTapMs = Long.MIN_VALUE / 2

        val isOn: Boolean get() = state != State.OFF

        fun tap(nowMs: Long) {
            state = when (state) {
                State.OFF -> State.ONCE
                State.ONCE -> if (nowMs - lastTapMs <= doubleTapMs) State.LOCKED else State.OFF
                State.LOCKED -> State.OFF
            }
            lastTapMs = nowMs
        }

        /** A key was typed: a one-time Shift is used up. */
        fun consume() {
            if (state == State.ONCE) state = State.OFF
        }

        fun reset() {
            state = State.OFF
        }
    }

    /**
     * Ctrl and Alt: tap = held for the next key, double tap = locked, a tap while on = off. Win: tap = held for the
     * next key; a second tap presses Win on its own (the Start menu) and releases it.
     */
    class Modifiers(private val doubleTapMs: Long = DOUBLE_TAP_MS) {
        private val held = mutableMapOf<Mod, Boolean>() // value: locked
        private val lastTap = mutableMapOf<Mod, Long>()

        fun isOn(mod: Mod) = mod in held
        fun isLocked(mod: Mod) = held[mod] == true
        val any: Boolean get() = held.isNotEmpty()

        /** Returns true when the tap means a bare Win key press. */
        fun tap(mod: Mod, nowMs: Long): Boolean {
            val last = lastTap[mod]
            lastTap[mod] = nowMs
            when {
                mod !in held -> held[mod] = false
                mod == Mod.WIN -> {
                    held.remove(mod)
                    return true
                }
                held[mod] == false && last != null && nowMs - last <= doubleTapMs -> held[mod] = true
                else -> held.remove(mod)
            }
            return false
        }

        /** A key went out with the modifiers: the unlocked ones are released. */
        fun consume() {
            held.entries.removeAll { !it.value }
        }

        fun reset() = held.clear()
    }

    /** What a key press sends. */
    sealed interface Output {
        data class Text(val text: String) : Output
        /** Press and release `vk`, with `ctrl`/`alt`/`shift` held (a plain key when none is) and Win around it. */
        data class Key(val vk: Int, val ctrl: Boolean = false, val alt: Boolean = false, val shift: Boolean = false, val win: Boolean = false) : Output
    }

    /** Win32 virtual key of a letter or digit (the same for every layout), or null. */
    fun vkOf(c: Char): Int? = when (c) {
        in 'a'..'z' -> c.uppercaseChar().code
        in 'A'..'Z' -> c.code
        in '0'..'9' -> c.code
        ' ' -> VK_SPACE
        else -> null
    }

    const val VK_SPACE = 0x20

    /**
     * What `key` sends with this Shift and these modifiers; null for keys that only change the keyboard (Shift,
     * modifiers, pages). Letters go as text unless Ctrl, Alt or Win is held, which turns them into key combos
     * (Ctrl+C) so shortcuts work whatever the host's layout.
     */
    fun output(key: Key, shift: Boolean, mods: Modifiers): Output? {
        val ctrl = mods.isOn(Mod.CTRL)
        val alt = mods.isOn(Mod.ALT)
        val win = mods.isOn(Mod.WIN)
        val combo = ctrl || alt || win
        return when (key.kind) {
            Kind.CHAR, Kind.SPACE -> {
                val vk = key.text.singleOrNull()?.let(::vkOf)
                if (combo && vk != null) Output.Key(vk, ctrl, alt, shift, win)
                else Output.Text(if (shift) key.text.uppercase() else key.text)
            }
            Kind.BACKSPACE -> Output.Key(WindowsKeys.VK_BACK, ctrl, alt, shift, win)
            Kind.ENTER -> Output.Key(WindowsKeys.VK_RETURN, ctrl, alt, shift, win)
            Kind.SPECIAL -> Output.Key(key.vk, ctrl, alt, shift, win)
            Kind.SHIFT, Kind.MOD, Kind.PAGE -> null
        }
    }

    /** Backspace repeat: first repeat after 400 ms, then every 50 ms. */
    fun repeatDelayMs(repeatsSoFar: Int): Long = if (repeatsSoFar == 0) 400L else 50L

    private const val DOUBLE_TAP_MS = 350L
}
