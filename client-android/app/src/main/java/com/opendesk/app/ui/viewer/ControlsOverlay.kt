package com.opendesk.app.ui.viewer

import android.content.res.ColorStateList
import android.view.HapticFeedbackConstants
import android.view.LayoutInflater
import android.view.View
import android.view.Gravity
import android.view.ViewGroup
import android.view.animation.OvershootInterpolator
import android.widget.FrameLayout
import android.widget.TextView
import androidx.core.content.ContextCompat
import androidx.core.content.res.ResourcesCompat
import androidx.core.widget.TextViewCompat
import com.opendesk.app.R
import com.opendesk.app.platform.audio.VoiceCallManager
import com.opendesk.app.session.RelayConnection

/**
 * The viewer's overlay (view_overlay): the session pill, the radial menu with its edge grip, the controls (a bottom
 * sheet in portrait, a side panel in landscape, opened from the menu's "More") and the key bar above the soft
 * keyboard. Nothing stays over the remote screen but the faint grip: the pill slides in on a swipe down from the top
 * ([showPill]) and hides again by itself. It is inflated again on rotation ([build]); [refresh] shows the current
 * [ViewerState].
 */
class ControlsOverlay(private val host: ViewGroup, private val state: ViewerState, private val listener: Listener) {

    /** A control tile or key; [Listener.onTool] carries it out. */
    enum class Tool {
        LEFT, DOUBLE, RIGHT, HOLD, ESC, TAB, WIN, KEYBOARD, HIDE_KEYBOARD, SCROLL, FIT, MIC, MONITOR,

        /** Switches between trackpad and touch mode. */
        MOUSE_MODE,

        /** Fit (the whole screen, with bands) or fill (no bands; the edges are reached by panning). */
        FIT_MODE, FILL_MODE, ALT_TAB, TASK_MANAGER,
    }

    interface Listener {
        fun onExit()
        fun onGestures()
        fun onTool(tool: Tool)
    }

    private val context get() = host.context
    private var overlay: View? = null
    private var pill: View? = null
    private var radial: RadialMenuView? = null
    private var hinted = false
    private val hidePill = Runnable { setPillShown(false) }

    /** Shown in the session pill. */
    var partnerName = ""
    var fps = 0

    fun build() {
        host.removeAllViews()
        val view = LayoutInflater.from(context).inflate(R.layout.view_overlay, host, false)
        host.addView(view)
        overlay = view
        sizeToolIcons(view)

        view.findViewById<View>(R.id.btn_exit_remote).setOnClickListener { listener.onExit() }
        view.findViewById<View>(R.id.tv_partner).setOnClickListener { listener.onGestures() }
        pill = view.findViewById<View>(R.id.top_pill).apply { visibility = View.INVISIBLE }
        buildRadial(view as ViewGroup)
        view.findViewById<View>(R.id.controls_handle).setOnClickListener { setOpen(false) }
        view.findViewById<View>(R.id.controls_title)?.setOnClickListener { listener.onGestures() }
        view.findViewById<View>(R.id.seg_trackpad).setOnClickListener { setTrackpad(true) }
        view.findViewById<View>(R.id.seg_touch).setOnClickListener { setTrackpad(false) }

        tool(R.id.btn_left_click, Tool.LEFT)
        tool(R.id.btn_double_click, Tool.DOUBLE)
        tool(R.id.btn_right_click, Tool.RIGHT, HapticFeedbackConstants.LONG_PRESS)
        tool(R.id.btn_hold_drag, Tool.HOLD, HapticFeedbackConstants.LONG_PRESS)
        tool(R.id.btn_key_esc, Tool.ESC)
        tool(R.id.btn_key_tab, Tool.TAB)
        tool(R.id.btn_key_win, Tool.WIN)
        tool(R.id.btn_keyboard, Tool.KEYBOARD)
        tool(R.id.btn_scroll, Tool.SCROLL)
        tool(R.id.btn_fit, Tool.FIT)
        tool(R.id.btn_toggle_mic, Tool.MIC)
        tool(R.id.btn_switch_monitor, Tool.MONITOR)
        tool(R.id.kb_esc, Tool.ESC)
        tool(R.id.kb_tab, Tool.TAB)
        tool(R.id.kb_win, Tool.WIN)
        tool(R.id.kb_hide, Tool.HIDE_KEYBOARD)
        refresh()
    }

    /** Opens or closes the controls. */
    fun setOpen(open: Boolean) {
        state.controlsOpen = open
        refresh()
    }

    // ---- Session pill ----

    /** Slides the session pill in from the top; it hides again after a few seconds. */
    fun showPill() = setPillShown(true)

    private fun setPillShown(shown: Boolean) {
        val p = pill ?: return
        val hiddenY = -(p.height + p.top).toFloat().coerceAtLeast(1f)
        p.removeCallbacks(hidePill)
        p.animate().cancel()
        if (shown) {
            if (p.visibility != View.VISIBLE) {
                p.translationY = hiddenY
                p.alpha = 0f
                p.visibility = View.VISIBLE
            }
            p.animate().translationY(0f).alpha(1f).setDuration(PILL_IN_MS).setInterpolator(OvershootInterpolator(1.1f)).start()
            p.postDelayed(hidePill, PILL_SHOWN_MS)
        } else if (p.visibility == View.VISIBLE) {
            p.animate().translationY(hiddenY).alpha(0f).setDuration(PILL_OUT_MS).withEndAction { p.visibility = View.INVISIBLE }.start()
        }
    }

    // ---- Radial menu ----

    /** Closes the radial menu; false when it was not open. */
    fun closeRadial(): Boolean {
        val r = radial ?: return false
        if (!r.isOpen) return false
        r.close()
        return true
    }

    private fun buildRadial(frame: ViewGroup) {
        val menu = RadialMenuView(context).apply { onPick = ::onPick }
        val handle = EdgeHandle(context, menu, ::radialItems)
        val density = context.resources.displayMetrics.density
        frame.addView(
            handle,
            FrameLayout.LayoutParams((HANDLE_W_DP * density).toInt(), (HANDLE_H_DP * density).toInt(), Gravity.END or Gravity.CENTER_VERTICAL),
        )
        // Above the grip, so a sticky menu takes the taps there too.
        frame.addView(menu, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        radial = menu
        if (!hinted) {
            hinted = true
            handle.hint()
        }
    }

    private fun onPick(item: RadialMenuView.Item) {
        when (val id = item.id) {
            is Tool -> listener.onTool(id)
            MENU_MORE -> setOpen(true)
            MENU_INFO -> showPill()
        }
    }

    /** The menu as it stands now: modes show their current state. */
    private fun radialItems(): List<RadialMenuView.Item> {
        fun s(id: Int) = context.getString(id)
        val trackpad = state.trackpadMode
        return listOf(
            RadialMenuView.Item(Tool.KEYBOARD, s(R.string.keyboard), R.drawable.ic_kbd, active = state.keyboardVisible),
            RadialMenuView.Item(
                Tool.MOUSE_MODE,
                s(if (trackpad) R.string.menu_touch else R.string.menu_trackpad),
                if (trackpad) R.drawable.ic_touch else R.drawable.ic_cursor,
            ),
            RadialMenuView.Item(
                MENU_DISPLAY,
                s(R.string.menu_display),
                R.drawable.ic_screen,
                children = listOf(
                    RadialMenuView.Item(Tool.FIT_MODE, s(R.string.menu_fit), R.drawable.ic_fit, active = !state.fill),
                    RadialMenuView.Item(Tool.FILL_MODE, s(R.string.menu_fill), R.drawable.ic_fill, active = state.fill),
                    RadialMenuView.Item(Tool.MONITOR, s(R.string.menu_next_monitor), R.drawable.ic_desktop),
                    RadialMenuView.Item(MENU_INFO, s(R.string.menu_info), R.drawable.ic_lock),
                ),
            ),
            RadialMenuView.Item(
                MENU_KEYS,
                s(R.string.menu_keys),
                R.drawable.ic_win,
                children = listOf(
                    RadialMenuView.Item(Tool.ESC, s(R.string.key_esc), glyph = s(R.string.key_esc)),
                    RadialMenuView.Item(Tool.TAB, s(R.string.key_tab), glyph = s(R.string.key_tab)),
                    RadialMenuView.Item(Tool.WIN, s(R.string.key_win), R.drawable.ic_win),
                    RadialMenuView.Item(Tool.ALT_TAB, s(R.string.menu_alt_tab), glyph = s(R.string.glyph_alt_tab)),
                    RadialMenuView.Item(Tool.TASK_MANAGER, s(R.string.menu_task_manager), glyph = s(R.string.glyph_task_manager)),
                ),
            ),
            RadialMenuView.Item(Tool.SCROLL, s(R.string.menu_scroll), R.drawable.ic_scroll, active = state.scrollMode),
            RadialMenuView.Item(MENU_MORE, s(R.string.menu_more), R.drawable.ic_gear),
        )
    }

    private fun setTrackpad(trackpad: Boolean) {
        state.trackpadMode = trackpad
        refresh()
    }

    private fun tool(id: Int, tool: Tool, haptic: Int = HapticFeedbackConstants.VIRTUAL_KEY) {
        overlay?.findViewById<View>(id)?.setOnClickListener {
            it.performHapticFeedback(haptic)
            listener.onTool(tool)
        }
    }

    /** Shows the state: which part is visible, the mode, active tools and the session pill. */
    fun refresh() {
        val view = overlay ?: return
        val panelOpen = state.controlsOpen && !state.keyboardVisible
        view.findViewById<View>(R.id.controls_panel).visibility = if (panelOpen) View.VISIBLE else View.GONE
        view.findViewById<View>(R.id.key_bar).visibility = if (state.keyboardVisible) View.VISIBLE else View.GONE

        segment(view.findViewById(R.id.seg_trackpad), state.trackpadMode)
        segment(view.findViewById(R.id.seg_touch), !state.trackpadMode)
        val muted = VoiceCallManager.isMuted
        view.findViewById<TextView>(R.id.btn_toggle_mic).apply {
            setText(if (muted) R.string.mic_off else R.string.mic_on)
            active(this, !muted)
        }
        active(view.findViewById(R.id.btn_hold_drag), state.leftHeld)
        active(view.findViewById(R.id.btn_scroll), state.scrollMode)
        refreshStatus()
    }

    /** The session pill: computer, frame rate and the E2E badge. */
    fun refreshStatus() {
        val view = overlay ?: return
        view.findViewById<TextView>(R.id.tv_partner).text = partnerName
        view.findViewById<TextView>(R.id.tv_remote_fps).text = context.getString(R.string.fps_label, fps)
        view.findViewById<View>(R.id.tv_e2e).visibility = if (RelayConnection.isEndToEnd) View.VISIBLE else View.GONE
    }

    private fun segment(view: TextView, on: Boolean) {
        view.setBackgroundResource(if (on) R.drawable.seg_on else 0)
        val color = ContextCompat.getColor(context, if (on) R.color.text else R.color.text3)
        view.setTextColor(color)
        TextViewCompat.setCompoundDrawableTintList(view, ColorStateList.valueOf(color))
        view.typeface = ResourcesCompat.getFont(context, if (on) R.font.inter_semibold else R.font.inter_regular)
    }

    /** A control tile that is on (held button, microphone, scroll mode) is tinted with the accent. */
    private fun active(view: TextView, on: Boolean) {
        view.setBackgroundResource(if (on) R.drawable.tool_tile_accent else R.drawable.tool_tile)
        val color = ContextCompat.getColor(context, if (on) R.color.accent_tx else R.color.text)
        view.setTextColor(color)
        TextViewCompat.setCompoundDrawableTintList(view, ColorStateList.valueOf(color))
    }

    private companion object {
        const val PILL_IN_MS = 320L
        const val PILL_OUT_MS = 180L
        const val PILL_SHOWN_MS = 3500L
        const val HANDLE_W_DP = 28f
        const val HANDLE_H_DP = 200f

        // Menu entries that are not a Tool: they open a level or act on the overlay itself.
        const val MENU_DISPLAY = "display"
        const val MENU_KEYS = "keys"
        const val MENU_MORE = "more"
        const val MENU_INFO = "info"
    }

    /** Compound icons of the control tiles at the design size (18 dp; 15 dp beside a label). */
    private fun sizeToolIcons(view: View) {
        val density = context.resources.displayMetrics.density
        val size = (18 * density).toInt()
        val small = (15 * density).toInt()
        fun walk(v: View) {
            if (v is ViewGroup) {
                for (i in 0 until v.childCount) walk(v.getChildAt(i))
            } else if (v is TextView) {
                val d = v.compoundDrawablesRelative
                if (d.all { it == null }) return
                val px = if (d[0] != null) small else size
                d.forEach { it?.setBounds(0, 0, px, px) }
                v.setCompoundDrawablesRelative(d[0], d[1], d[2], d[3])
            }
        }
        walk(view)
    }
}
