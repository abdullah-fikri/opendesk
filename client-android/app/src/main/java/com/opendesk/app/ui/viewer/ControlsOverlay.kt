package com.opendesk.app.ui.viewer

import android.content.res.ColorStateList
import android.view.HapticFeedbackConstants
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.TextView
import androidx.core.content.ContextCompat
import androidx.core.content.res.ResourcesCompat
import androidx.core.widget.TextViewCompat
import com.opendesk.app.R
import com.opendesk.app.platform.audio.VoiceCallManager
import com.opendesk.app.session.RelayConnection

/**
 * The viewer's overlay (view_overlay): the session pill, the Controls button, the controls (a bottom sheet in
 * portrait, a side panel in landscape) and the key bar above the soft keyboard. It is inflated again on rotation
 * ([build]); [refresh] shows the current [ViewerState].
 */
class ControlsOverlay(private val host: ViewGroup, private val state: ViewerState, private val listener: Listener) {

    /** A control tile or key; [Listener.onTool] carries it out. */
    enum class Tool { LEFT, DOUBLE, RIGHT, HOLD, ESC, TAB, WIN, KEYBOARD, HIDE_KEYBOARD, SCROLL, FIT, MIC, MONITOR }

    interface Listener {
        fun onExit()
        fun onGestures()
        fun onTool(tool: Tool)
    }

    private val context get() = host.context
    private var overlay: View? = null

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
        view.findViewById<View>(R.id.btn_controls).apply {
            setOnClickListener { setOpen(true) }
            setOnLongClickListener {
                listener.onGestures()
                true
            }
        }
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
        view.findViewById<View>(R.id.btn_controls).visibility = if (panelOpen || state.keyboardVisible) View.GONE else View.VISIBLE
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
