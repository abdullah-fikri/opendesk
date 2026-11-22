package com.opendesk.app.ui.viewer.keyboard

import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.MotionEvent
import android.view.TextureView
import android.view.View
import android.widget.Toast
import com.opendesk.app.R
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.FocusPan
import com.opendesk.app.ui.viewer.FocusPanner
import com.opendesk.app.ui.viewer.RemoteInput
import com.opendesk.app.ui.viewer.RemoteKeyboardView
import com.opendesk.app.ui.viewer.ViewerState

/**
 * The viewer's two keyboards: the mini glass keyboard ([GlassKeyboardView], the default) and the phone's own
 * ([RemoteKeyboardView], for other languages and voice typing). Raises and lowers them on the host's `text-focus`
 * events ([AutoKeyboard]) and on the Keyboard control, keeps the glass keyboard's place (and landscape split) per
 * orientation, pastes the phone's clipboard, and has the [FocusPanner] keep the focused field above the keyboard.
 */
class KeyboardController(
    private val glass: GlassKeyboardView,
    private val ime: RemoteKeyboardView,
    private val input: RemoteInput,
    private val settings: SettingsRepository,
    private val state: ViewerState,
    /** Video and JPEG layers of the remote screen, for the glass backdrop. */
    private val video: TextureView,
    private val screen: View,
    private val panner: FocusPanner,
    /** A keyboard came up or went down (the controls show it). */
    private val onChanged: () -> Unit,
) {
    private val auto = AutoKeyboard()
    private val handler = Handler(Looper.getMainLooper())
    private val autoHide = Runnable {
        auto.onHidden()
        hideAll()
    }
    /** A tap into a field that already had focus (see [AutoKeyboard.showAfterTap]). */
    private val retapCheck = Runnable {
        if (settings.autoKeyboard && auto.showAfterTap(anyShown)) showPreferred()
    }
    private var downAt = 0L
    private var downX = 0f
    private var downY = 0f
    private val touchSlop = android.view.ViewConfiguration.get(glass.context).scaledTouchSlop
    /** Glass to phone keyboard in progress: no keyboard for a moment, but the field should not move back. */
    private var switchingToIme = false
    private val switchTimeout = Runnable {
        switchingToIme = false
        updatePanner()
    }
    private val clipboard = glass.context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    private val clipListener = ClipboardManager.OnPrimaryClipChangedListener { refreshPaste() }

    private val landscape: Boolean
        get() = glass.resources.configuration.orientation == Configuration.ORIENTATION_LANDSCAPE

    private val tablet: Boolean
        get() = glass.resources.configuration.smallestScreenWidthDp >= TABLET_DP

    /** Some keyboard is up. */
    val anyShown: Boolean get() = glass.isUp || state.keyboardVisible

    fun attach() {
        applyPlacement()
        glass.backdrop = ::fillBackdrop
        clipboard.addPrimaryClipChangedListener(clipListener)
        refreshPaste()
        glass.listener = object : GlassKeyboardView.Listener {
            override fun onText(text: String) = input.text(text)

            override fun onKey(key: KeyInput.Output.Key) = input.combo(key.vk, key.ctrl, key.alt, key.shift, key.win)

            override fun onWinKey() = input.winKey()

            override fun onSystemKeyboard() {
                auto.onManual()
                // The phone's keyboard takes over: the screen keeps its place until it is up.
                switchingToIme = true
                handler.removeCallbacks(switchTimeout)
                handler.postDelayed(switchTimeout, IME_SWITCH_MS)
                showGlass(false)
                ime.setKeyboardVisible(true)
            }

            override fun onHide() {
                auto.onManual()
                showGlass(false)
            }

            override fun onPaste() = paste()

            override fun onSplitChanged(split: Boolean) {
                settings.setKeyboardSplit(split)
                updatePanner()
            }

            override fun onPlacementChanged(placement: GlassKeyboardView.Placement) {
                settings.setKeyboardPlacement(
                    landscape,
                    placement.scale,
                    placement.centerX,
                    placement.bottomGap,
                    placement.opacity,
                )
                updatePanner()
            }
        }
    }

    /** Rotation: the glass keyboard goes where the user left it in this orientation. */
    fun onConfigurationChanged() {
        applyPlacement()
        // The keyboard's new place is known once the views are laid out for the new orientation.
        glass.post { updatePanner() }
    }

    private fun applyPlacement() {
        val p = settings.keyboardPlacement(landscape)
        glass.split = settings.keyboardSplit(KeyboardLayouts.defaultSplit(tablet))
        glass.placement = GlassKeyboardView.Placement(p[0], p[1], p[2], p[3])
    }

    /** Every touch on the viewer: a tap shortly before a `text-focus` raises the keyboard. */
    fun onTouch(event: MotionEvent) {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downAt = SystemClock.uptimeMillis()
                downX = event.x
                downY = event.y
                if (!glass.onPanel(event.x, event.y)) panner.onUserTouch()
            }
            MotionEvent.ACTION_UP -> {
                val now = SystemClock.uptimeMillis()
                val tap = now - downAt < TAP_MAX_MS &&
                    kotlin.math.hypot(event.x - downX, event.y - downY) < touchSlop &&
                    !glass.onPanel(event.x, event.y)
                if (!tap) return
                auto.onTap(now)
                handler.removeCallbacks(retapCheck)
                if (auto.focused && !anyShown && settings.autoKeyboard) {
                    handler.postDelayed(retapCheck, AutoKeyboard.RETAP_CHECK_MS)
                }
            }
        }
    }

    /** The host's keyboard focus entered or left a text field (at `field` on the remote screen, when known). */
    fun onTextFocus(editable: Boolean, password: Boolean, field: FocusPan.Field?) {
        if (editable) glass.password = password
        panner.onField(if (editable) field else null)
        if (!editable) handler.removeCallbacks(retapCheck)
        if (!settings.autoKeyboard) return
        when (auto.onFocus(editable, anyShown, SystemClock.uptimeMillis())) {
            AutoKeyboard.Decision.SHOW -> {
                handler.removeCallbacks(autoHide)
                handler.removeCallbacks(retapCheck)
                showPreferred()
            }
            AutoKeyboard.Decision.HIDE_LATER -> {
                handler.removeCallbacks(autoHide)
                handler.postDelayed(autoHide, AutoKeyboard.HIDE_DELAY_MS)
            }
            AutoKeyboard.Decision.CANCEL_HIDE -> handler.removeCallbacks(autoHide)
            AutoKeyboard.Decision.NONE -> Unit
        }
    }

    private fun showPreferred() {
        if (settings.miniKeyboard) showGlass(true) else ime.setKeyboardVisible(true)
    }

    /** The Keyboard control: closes the open keyboard, or opens the preferred one. */
    fun toggle() {
        auto.onManual()
        handler.removeCallbacks(autoHide)
        when {
            glass.isUp -> showGlass(false)
            state.keyboardVisible -> ime.setKeyboardVisible(false)
            settings.miniKeyboard -> showGlass(true)
            else -> ime.setKeyboardVisible(true)
        }
    }

    fun hideAll() {
        handler.removeCallbacks(autoHide)
        showGlass(false)
        if (state.keyboardVisible) ime.setKeyboardVisible(false)
    }

    /** The phone's keyboard came up or went down; both never show at once. */
    fun onImeVisible(visible: Boolean) {
        if (visible && glass.isUp) showGlass(false) else updatePanner()
    }

    /** Back: closes the glass keyboard first. */
    fun handleBack(): Boolean {
        if (!glass.isUp) return false
        auto.onManual()
        showGlass(false)
        return true
    }

    fun release() {
        handler.removeCallbacks(autoHide)
        handler.removeCallbacks(retapCheck)
        handler.removeCallbacks(switchTimeout)
        clipboard.removePrimaryClipChangedListener(clipListener)
        panner.release()
    }

    private fun showGlass(shown: Boolean) {
        if (shown == glass.isUp) return
        if (!shown) glass.resetModifiers()
        if (shown) refreshPaste()
        glass.setShown(shown)
        state.glassKeyboardVisible = shown
        onChanged()
        updatePanner()
        // A keyboard shown for the first time has its place only after its first layout.
        if (shown) glass.post { updatePanner() }
    }

    /**
     * Tells the panner where the keyboard's top is: the glass panel's top, or for the phone's keyboard (which
     * shrinks the screen view) the view's bottom; null when no keyboard is up.
     */
    private fun updatePanner() {
        val top = glass.keyboardTop ?: if (state.keyboardVisible) screen.height.toFloat() else null
        if (top != null) {
            switchingToIme = false
            handler.removeCallbacks(switchTimeout)
        } else if (switchingToIme) {
            return
        }
        panner.onKeyboard(top)
    }

    // ---- Paste ----

    /** The paste button shows while the phone's clipboard holds text (reading only its description: no toast). */
    private fun refreshPaste() {
        val description = if (clipboard.hasPrimaryClip()) clipboard.primaryClipDescription else null
        glass.pasteAvailable = description != null &&
            (description.hasMimeType(ClipDescription.MIMETYPE_TEXT_PLAIN) || description.hasMimeType("text/*"))
    }

    /** Types the phone's clipboard text on the remote computer. */
    private fun paste() {
        val context = glass.context
        val clip = clipboard.primaryClip
        val text = if (clip != null && clip.itemCount > 0) clip.getItemAt(0).coerceToText(context)?.toString().orEmpty() else ""
        if (text.isEmpty()) {
            glass.pasteAvailable = false
            Toast.makeText(context, R.string.paste_empty, Toast.LENGTH_SHORT).show()
            return
        }
        val normalized = text.replace("\r\n", "\n").replace('\r', '\n')
        val sent = normalized.take(MAX_PASTE_CHARS)
        input.text(sent)
        val message = if (sent.length < normalized.length) {
            context.getString(R.string.paste_truncated, MAX_PASTE_CHARS)
        } else {
            context.getString(R.string.paste_sent)
        }
        Toast.makeText(context, message, Toast.LENGTH_SHORT).show()
    }

    /** The remote picture behind the keyboard: the H.264 layer, then the JPEG layer over it (transparent while
     *  H.264 shows). */
    private fun fillBackdrop(bitmap: Bitmap): Boolean {
        if (screen.width == 0 || screen.height == 0) return false
        bitmap.eraseColor(Color.BLACK)
        return try {
            if (video.isAvailable) video.getBitmap(bitmap)
            val canvas = Canvas(bitmap)
            canvas.scale(bitmap.width / screen.width.toFloat(), bitmap.height / screen.height.toFloat())
            screen.draw(canvas)
            true
        } catch (e: RuntimeException) {
            // A hardware bitmap or a released surface: plain tint this time.
            false
        }
    }

    private companion object {
        /** Longer than this, or moving, is a drag or a hold, not a tap into a field. */
        const val TAP_MAX_MS = 350L

        /** Smallest width of a tablet (its landscape keyboard is whole by default). */
        const val TABLET_DP = 600

        /** A paste larger than this is cut (typed as text events, a huge one would flood the connection). */
        const val MAX_PASTE_CHARS = 20_000

        /** How long the phone's keyboard may take to come up after the glass one went down for it. */
        const val IME_SWITCH_MS = 800L
    }
}
