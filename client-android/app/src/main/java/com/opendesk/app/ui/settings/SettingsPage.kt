package com.opendesk.app.ui.settings

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.TextView
import android.widget.Toast
import com.opendesk.app.OpenDeskApp
import com.opendesk.app.R
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.GatewayUrl
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Ui
import com.opendesk.app.ui.main.MainHost

/** Settings tab: relay, security, this phone (ID, microphone, administrator mode) and about. */
class SettingsPage(private val host: MainHost, parent: ViewGroup) {
    val view: View = LayoutInflater.from(parent.context).inflate(R.layout.page_settings, parent, false)
    private val context = view.context

    init {
        val settings = host.settings
        Ui.settingRow(
            view.findViewById(R.id.set_security), R.drawable.tile_green, R.drawable.ic_lock,
            context.getString(R.string.security), context.getString(R.string.security_sub), chevron = true,
        ) { host.showSecurity() }

        val idRow = view.findViewById<View>(R.id.set_device_id)
        Ui.settingRow(idRow, R.drawable.tile_purple, R.drawable.ic_phone, context.getString(R.string.this_device_id), settings.deskId)
        idRow.findViewById<TextView>(R.id.set_sub).apply {
            typeface = Ui.mono(context)
            textSize = 14f
            setTextColor(Ui.color(context, R.color.text2))
        }
        idRow.findViewById<TextView>(R.id.set_button).apply {
            visibility = View.VISIBLE
            setText(R.string.copy)
            setOnClickListener {
                val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                clipboard.setPrimaryClip(ClipData.newPlainText(context.getString(R.string.clip_label_id), settings.deskId))
                Toast.makeText(context, R.string.id_copied, Toast.LENGTH_SHORT).show()
            }
        }

        Ui.settingRow(
            view.findViewById(R.id.set_mic), R.drawable.tile_amber, R.drawable.ic_mic,
            context.getString(R.string.microphone), context.getString(R.string.microphone_sub),
        )

        val adminRow = view.findViewById<View>(R.id.set_admin)
        val adminSwitch = Ui.settingSwitch(adminRow)
        Ui.settingRow(
            adminRow, R.drawable.tile_blue, R.drawable.ic_desktop,
            context.getString(R.string.admin_mode), context.getString(R.string.admin_mode_sub),
        ) { adminSwitch.toggle() }
        adminSwitch.isChecked = settings.adminMode
        adminSwitch.setOnCheckedChangeListener { _, checked ->
            if (settings.adminMode == checked) return@setOnCheckedChangeListener
            settings.adminMode = checked
            // The role is sent at registration: reconnect so the relay sees it (and sends its registry).
            host.restartRelay()
        }

        val choices = listOf(
            SettingsRepository.APPEARANCE_SYSTEM to R.id.appearance_system,
            SettingsRepository.APPEARANCE_LIGHT to R.id.appearance_light,
            SettingsRepository.APPEARANCE_DARK to R.id.appearance_dark,
        )
        choices.forEach { (value, id) ->
            Ui.segment(view.findViewById(id), settings.appearance == value)
            view.findViewById<View>(id).setOnClickListener {
                if (settings.appearance == value) return@setOnClickListener
                settings.appearance = value
                choices.forEach { (v, i) -> Ui.segment(view.findViewById(i), v == value) }
                // Recreates the open screens in the new theme (this page is restored).
                OpenDeskApp.applyAppearance(value)
            }
        }

        val about = view.findViewById<View>(R.id.set_viewer_only)
        Ui.settingRow(about, R.drawable.tile_cyan, R.drawable.ic_phone, context.getString(R.string.viewer_only), context.getString(R.string.viewer_only_note))

        val version = runCatching { context.packageManager.getPackageInfo(context.packageName, 0).versionName }.getOrNull()
        view.findViewById<TextView>(R.id.settings_version).text = version?.let { context.getString(R.string.version_label, it) }
        refresh()
    }

    fun refresh() {
        val url = host.settings.gatewayUrl
        val relayRow = view.findViewById<View>(R.id.set_relay)
        Ui.settingRow(
            relayRow, R.drawable.tile_blue, R.drawable.ic_server, context.getString(R.string.relay_button),
            if (url.isEmpty()) context.getString(R.string.relay_not_configured) else displayUrl(url), chevron = true,
        ) { host.showRelay() }
        relayRow.findViewById<View>(R.id.set_check).visibility = if (RelayConnection.online) View.VISIBLE else View.GONE
    }

    companion object {
        /** "relay.example.org" for the usual wss:// address; anything else in full. */
        fun displayUrl(url: String): String = if (url.startsWith("wss://")) url.removePrefix("wss://") else url

        /** Whether `url` (normalized) is allowed: wss://, or ws:// where cleartext is permitted (loopback). */
        fun permitted(url: String): Boolean =
            !GatewayUrl.isPlaintext(url) ||
                android.security.NetworkSecurityPolicy.getInstance().isCleartextTrafficPermitted(GatewayUrl.host(url))
    }
}
