package com.opendesk.app.ui.viewer

import android.content.Context
import android.text.InputType
import android.util.AttributeSet
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import com.opendesk.app.domain.WindowsKeys

/**
 * Invisible view that owns the soft keyboard while the viewer types on the remote machine. Characters go to
 * [onText]; Backspace, Enter, arrows and the like go to [onKey] as Win32 virtual keys. Composing text (word
 * suggestions) is mirrored on the host as it changes, so what the user sees in the suggestion strip is what the
 * host receives.
 */
class RemoteKeyboardView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : View(context, attrs) {

    /** Typed characters. */
    var onText: (String) -> Unit = {}

    /** A non-printing key, pressed and released (Win32 virtual-key code). */
    var onKey: (Int) -> Unit = {}

    init {
        isFocusable = true
        isFocusableInTouchMode = true
    }

    /** Shows the soft keyboard (`true`) or hides it. */
    fun setKeyboardVisible(visible: Boolean) {
        val imm = context.getSystemService(InputMethodManager::class.java) ?: return
        if (visible) {
            requestFocus()
            imm.restartInput(this)
            imm.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
        } else {
            imm.hideSoftInputFromWindow(windowToken, 0)
            clearFocus()
        }
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        // No suggestions or autocorrect: the remote side would receive every correction as retyping.
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or
            InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN or
            EditorInfo.IME_ACTION_NONE
        return Connection()
    }

    /** Hardware keyboards and IMEs that send key events instead of text. */
    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        val vk = WindowsKeys.fromAndroid(keyCode)
        if (vk != null) {
            onKey(vk)
            return true
        }
        val ch = event.unicodeChar
        if (ch != 0 && !event.isCtrlPressed && !event.isAltPressed) {
            onText(String(Character.toChars(ch)))
            return true
        }
        return super.onKeyDown(keyCode, event)
    }

    private inner class Connection : BaseInputConnection(this, false) {
        /** What the IME is composing right now, as already typed on the host. */
        private var composing = ""

        override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
            replaceComposing(text.toString())
            composing = ""
            return true
        }

        override fun setComposingText(text: CharSequence, newCursorPosition: Int): Boolean {
            replaceComposing(text.toString())
            composing = text.toString()
            return true
        }

        override fun finishComposingText(): Boolean {
            composing = ""
            return true
        }

        override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
            repeat(beforeLength.coerceAtMost(MAX_REPEAT)) { onKey(WindowsKeys.VK_BACK) }
            repeat(afterLength.coerceAtMost(MAX_REPEAT)) { onKey(WindowsKeys.VK_DELETE) }
            return true
        }

        override fun sendKeyEvent(event: KeyEvent): Boolean {
            if (event.action == KeyEvent.ACTION_DOWN) onKeyDown(event.keyCode, event)
            return true
        }

        override fun performEditorAction(actionCode: Int): Boolean {
            onKey(WindowsKeys.VK_RETURN)
            return true
        }

        private fun replaceComposing(next: String) {
            val (erase, type) = WindowsKeys.diff(composing, next)
            repeat(erase.coerceAtMost(MAX_REPEAT)) { onKey(WindowsKeys.VK_BACK) }
            if (type.isNotEmpty()) onText(type)
        }
    }

    private companion object {
        /** An IME can ask to delete a whole paragraph; never flood the host. */
        const val MAX_REPEAT = 64
    }
}
