package com.opendesk.app.ui.settings

import android.app.Activity
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.TextView
import com.opendesk.app.R
import com.opendesk.app.domain.DeskId
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Alerts
import com.opendesk.app.ui.common.Ui
import com.opendesk.app.ui.main.MainHost
import java.util.Locale

/** Security: encryption (always on), this phone's key, the computers with a pinned key, and the device token. */
class SecurityPage(private val activity: Activity, private val host: MainHost, parent: ViewGroup) {
    val view: View = LayoutInflater.from(parent.context).inflate(R.layout.page_security, parent, false)
    private val context = view.context
    private val trusted: LinearLayout = view.findViewById(R.id.trusted_list)

    init {
        view.findViewById<View>(R.id.page_back).setOnClickListener { host.showSettings() }

        val e2e = view.findViewById<View>(R.id.sec_e2e)
        Ui.settingRow(e2e, R.drawable.tile_green, R.drawable.ic_lock, context.getString(R.string.e2e_title), context.getString(R.string.e2e_sub))
        Ui.settingSwitch(e2e).apply {
            // Always on: shown, not changeable.
            isChecked = true
            isClickable = false
            isFocusable = false
        }

        val key = view.findViewById<View>(R.id.sec_key)
        Ui.settingRow(
            key, R.drawable.tile_purple, R.drawable.ic_lock, context.getString(R.string.this_phone_key),
            RelayConnection.keyFingerprint?.uppercase(Locale.ROOT) ?: "—",
        )
        key.findViewById<TextView>(R.id.set_sub).typeface = Ui.mono(context)
        refresh()
    }

    fun refresh() {
        trusted.removeAllViews()
        val pinned = RelayConnection.pinnedHosts()
        if (pinned.isEmpty()) {
            trusted.addView(
                TextView(context).apply {
                    setText(R.string.trusted_none)
                    setTextColor(Ui.color(context, R.color.text3))
                    textSize = 14f
                    val pad = Ui.dp(context, 16f)
                    setPadding(pad, pad, pad, pad)
                },
            )
        }
        pinned.forEach { id ->
            val item = host.directory.lookup(id)
            val row = Ui.inflate(trusted, R.layout.row_trusted)
            row.findViewById<TextView>(R.id.row_title).text = item.name
            row.findViewById<TextView>(R.id.row_sub).text = DeskId.format(id)
            row.setOnClickListener {
                Alerts.show(
                    activity,
                    context.getString(R.string.forget_key_title),
                    context.getString(R.string.forget_key_body, item.name),
                    context.getString(R.string.forget),
                    context.getString(R.string.cancel),
                    onPrimary = {
                        RelayConnection.trustHost(id)
                        refresh()
                    },
                )
            }
            Ui.addRow(trusted, row)
        }

        val issued = host.settings.deviceTokenRelay.isNotEmpty() && host.settings.tokenForGateway.isNotEmpty()
        Ui.settingRow(
            view.findViewById(R.id.sec_token), R.drawable.tile_amber, R.drawable.ic_lock,
            context.getString(R.string.device_token),
            context.getString(if (issued) R.string.device_token_issued else R.string.device_token_sub),
            chevron = true,
        ) { host.showRelay() }
    }
}
