package com.opendesk.app.ui.devices

import android.content.res.ColorStateList
import android.graphics.Color
import android.view.LayoutInflater
import android.view.View
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.google.android.material.bottomsheet.BottomSheetBehavior
import com.google.android.material.bottomsheet.BottomSheetDialog
import com.opendesk.app.R
import com.opendesk.app.domain.DeviceItem
import com.opendesk.app.ui.common.Ui
import com.opendesk.app.ui.main.ConnectState
import com.opendesk.app.ui.main.MainHost

/** Fills a `view_connect_state` include: ready (presence and encryption), connecting, or the failure. */
object ConnectStateView {
    fun bind(view: View, item: DeviceItem, state: ConnectState, showReady: Boolean = true) {
        val ready = view.findViewById<View>(R.id.state_ready)
        val connecting = view.findViewById<View>(R.id.state_connecting)
        val error = view.findViewById<View>(R.id.state_error)
        ready.visibility = if (state == ConnectState.Ready && showReady) View.VISIBLE else View.GONE
        connecting.visibility = if (state == ConnectState.Connecting) View.VISIBLE else View.GONE
        error.visibility = if (state is ConnectState.Failed) View.VISIBLE else View.GONE
        view.visibility = if (ready.visibility == View.GONE && connecting.visibility == View.GONE && error.visibility == View.GONE) {
            View.GONE
        } else {
            View.VISIBLE
        }
        if (state is ConnectState.Failed) view.findViewById<TextView>(R.id.state_error_body).text = state.message

        val context = view.context
        val presence = view.findViewById<TextView>(R.id.state_presence)
        val dot = view.findViewById<View>(R.id.state_dot)
        val (label, tone) = when (item.online) {
            true -> R.string.presence_online to R.color.green
            false -> R.string.presence_offline to R.color.offline
            null -> R.string.presence_unknown to R.color.text3
        }
        presence.setText(label)
        presence.setTextColor(Ui.color(context, tone))
        dot.backgroundTintList = ColorStateList.valueOf(Ui.color(context, tone))
    }
}

/** Phone: the bottom sheet that connects to one computer (ready, connecting, failed; success opens the viewer). */
class ConnectSheet(private val activity: AppCompatActivity, private val host: MainHost) {
    private var dialog: BottomSheetDialog? = null
    private var view: View? = null
    private var item: DeviceItem? = null

    val isShowing: Boolean get() = dialog?.isShowing == true

    fun show(item: DeviceItem) {
        this.item = item
        if (dialog?.isShowing != true) {
            val content = LayoutInflater.from(activity).inflate(R.layout.sheet_connect, null)
            ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
                val bottom = insets.getInsets(WindowInsetsCompat.Type.systemBars()).bottom
                v.setPadding(v.paddingLeft, v.paddingTop, v.paddingRight, Ui.dp(activity, 22f) + bottom)
                insets
            }
            val sheet = BottomSheetDialog(activity)
            sheet.setContentView(content)
            sheet.setOnShowListener {
                (content.parent as? View)?.setBackgroundColor(Color.TRANSPARENT)
                sheet.behavior.state = BottomSheetBehavior.STATE_EXPANDED
                sheet.behavior.skipCollapsed = true
            }
            sheet.setOnDismissListener {
                if (host.connectState == ConnectState.Connecting && host.target?.cleanId == this.item?.cleanId) host.cancelConnect()
                if (dialog === sheet) {
                    dialog = null
                    view = null
                }
            }
            dialog = sheet
            view = content
            sheet.show()
        }
        refresh()
    }

    fun dismiss() {
        dialog?.dismiss()
    }

    fun refresh() {
        val view = view ?: return
        val item = item ?: return
        val shown = if (host.target?.cleanId == item.cleanId) host.target ?: item else item
        val state = if (host.target?.cleanId == item.cleanId) host.connectState else ConnectState.Ready
        view.findViewById<TextView>(R.id.sheet_name).text = shown.name
        view.findViewById<TextView>(R.id.sheet_id).text =
            if (shown.platform.isEmpty()) shown.formattedId else activity.getString(R.string.id_platform, shown.formattedId, shown.platform)
        ConnectStateView.bind(view.findViewById(R.id.sheet_state), shown, state)
        view.findViewById<TextView>(R.id.sheet_button).apply {
            when (state) {
                ConnectState.Connecting -> {
                    setText(R.string.cancel)
                    setBackgroundResource(R.drawable.btn_neutral)
                    setTextColor(Ui.color(activity, R.color.text))
                    setOnClickListener {
                        host.cancelConnect()
                        refresh()
                    }
                }
                else -> {
                    setText(if (state is ConnectState.Failed) R.string.try_again else R.string.connect_to_pc)
                    setBackgroundResource(R.drawable.btn_accent_large)
                    setTextColor(Ui.color(activity, R.color.on_accent))
                    setOnClickListener { host.connect(shown) }
                }
            }
        }
    }
}
