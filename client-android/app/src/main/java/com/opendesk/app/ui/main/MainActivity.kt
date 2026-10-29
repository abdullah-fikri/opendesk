package com.opendesk.app.ui.main

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.security.NetworkSecurityPolicy
import android.text.InputType
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import com.opendesk.app.R
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.GatewayUrl
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.viewer.RemoteViewerActivity

/** Home screen: the relay connection, connecting to a computer, and this phone's ID. */
class MainActivity : AppCompatActivity(), RelayConnection.Listener {

    private lateinit var settings: SettingsRepository
    private lateinit var tvStatus: TextView
    private lateinit var tvGatewayUrl: TextView
    private lateinit var etTargetId: EditText
    private lateinit var btnConnect: Button

    /** A connect-request is waiting for the relay's answer. */
    private var awaitingSession = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        settings = SettingsRepository(this)

        tvStatus = findViewById(R.id.tv_status)
        tvGatewayUrl = findViewById(R.id.tv_gateway_url)
        etTargetId = findViewById(R.id.et_target_id)
        btnConnect = findViewById(R.id.btn_connect)

        val deskId = settings.deskId
        findViewById<TextView>(R.id.tv_my_id).text = deskId
        findViewById<Button>(R.id.btn_copy_id).setOnClickListener {
            val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            clipboard.setPrimaryClip(ClipData.newPlainText(getString(R.string.clip_label_id), deskId))
            toast(R.string.id_copied)
        }
        findViewById<Button>(R.id.btn_settings_gateway).setOnClickListener { showGatewaySettingsDialog() }
        btnConnect.setOnClickListener { connectToTarget() }
        etTargetId.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == EditorInfo.IME_ACTION_GO) {
                connectToTarget()
                true
            } else {
                false
            }
        }

        RelayConnection.listener = this
        // The connection is process-wide: a recreated activity (configuration change) keeps the running one.
        if (!RelayConnection.isStarted) {
            connectRelay()
        } else {
            showRelayUrl()
            onRelayStatus(RelayConnection.online)
        }
    }

    override fun onStart() {
        super.onStart()
        RelayConnection.listener = this
        RelayConnection.retryNowIfOffline()
        onRelayStatus(RelayConnection.online)
        // The session may have started while this screen was in the background.
        if (RelayConnection.viewerPartner.isNotEmpty() && !RemoteViewerActivity.isOpen) openViewer()
    }

    override fun onDestroy() {
        if (RelayConnection.listener === this) RelayConnection.listener = null
        // Leaving the app closes the connection; a configuration change does not.
        if (isFinishing && !isChangingConfigurations) RelayConnection.stop()
        super.onDestroy()
    }

    // ---- Relay ----

    private fun showRelayUrl() {
        val url = settings.gatewayUrl
        tvGatewayUrl.text = getString(R.string.relay_label, url.ifEmpty { getString(R.string.relay_not_configured) })
    }

    private fun connectRelay() {
        showRelayUrl()
        val url = settings.gatewayUrl
        if (url.isNotEmpty() && !isUrlPermitted(url)) {
            toast(R.string.relay_plaintext_refused)
            showGatewaySettingsDialog()
            return
        }
        tvStatus.setText(R.string.status_connecting)
        if (!RelayConnection.start(this, settings)) {
            onRelayStatus(false)
            showGatewaySettingsDialog()
        }
    }

    /** Plain ws:// works only where the network security config allows cleartext (loopback in release builds). */
    private fun isUrlPermitted(url: String): Boolean =
        !GatewayUrl.isPlaintext(url) ||
            NetworkSecurityPolicy.getInstance().isCleartextTrafficPermitted(GatewayUrl.host(url))

    private fun connectToTarget() {
        val target = etTargetId.text.toString().trim()
        if (target.isEmpty()) {
            toast(R.string.enter_target_id)
            return
        }
        if (!RelayConnection.connectTo(target)) {
            toast(R.string.relay_offline)
            return
        }
        awaitingSession = true
        btnConnect.isEnabled = false
        btnConnect.setText(R.string.connecting)
    }

    private fun resetConnectButton() {
        awaitingSession = false
        btnConnect.isEnabled = true
        btnConnect.setText(R.string.connect_to_pc)
    }

    private fun showGatewaySettingsDialog() {
        val urlInput = EditText(this).apply {
            setText(settings.gatewayUrl)
            setSelection(text.length)
            hint = getString(R.string.relay_url_hint)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
        }
        val fields = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val padding = resources.getDimensionPixelSize(R.dimen.dialog_padding)
            setPadding(padding, padding, padding, 0)
            addView(urlInput)
        }
        val message = getString(R.string.relay_settings_message) +
            (RelayConnection.keyFingerprint?.let { "\n\n" + getString(R.string.this_device_key, it) } ?: "")
        AlertDialog.Builder(this)
            .setTitle(R.string.relay_settings_title)
            .setMessage(message)
            .setView(fields)
            .setPositiveButton(R.string.save_and_connect) { _, _ ->
                val url = GatewayUrl.normalize(urlInput.text.toString())
                // Nothing else to enter: a relay that requires tokens issues this phone's own on first connect.
                if (url.isEmpty()) {
                    toast(R.string.relay_settings_invalid)
                    return@setPositiveButton
                }
                if (!isUrlPermitted(url)) {
                    toast(R.string.relay_plaintext_refused)
                    return@setPositiveButton
                }
                settings.gatewayUrl = url
                connectRelay()
                toast(R.string.relay_updated)
            }
            .setNegativeButton(R.string.cancel, null)
            .show()
    }

    private fun openViewer() {
        startActivity(Intent(this, RemoteViewerActivity::class.java))
    }

    // ---- RelayConnection.Listener ----

    override fun onRelayStatus(online: Boolean) {
        val text = when {
            online -> R.string.status_connected
            settings.gatewayUrl.isEmpty() -> R.string.status_disconnected
            else -> R.string.status_reconnecting
        }
        tvStatus.setText(text)
        tvStatus.setTextColor(ContextCompat.getColor(this, if (online) R.color.status_online else R.color.status_offline))
        // A request sent before the connection dropped will never be answered.
        if (!online && awaitingSession) resetConnectButton()
    }

    override fun onViewerConnected(partnerClean: String) {
        resetConnectButton()
        openViewer()
    }

    override fun onSessionClosed(message: String?) {
        resetConnectButton()
        Toast.makeText(this, message ?: getString(R.string.session_closed), Toast.LENGTH_SHORT).show()
    }

    override fun onError(message: String?) {
        resetConnectButton()
        Toast.makeText(this, message ?: getString(R.string.error_generic), Toast.LENGTH_SHORT).show()
    }

    override fun onNotice(text: String) {
        Toast.makeText(this, text, Toast.LENGTH_SHORT).show()
    }

    override fun onSecurityAlert(hostClean: String, message: String) {
        resetConnectButton()
        if (isFinishing || isDestroyed) return
        AlertDialog.Builder(this)
            .setTitle(R.string.security_alert_title)
            .setMessage(message)
            .setPositiveButton(R.string.cancel, null)
            .setNegativeButton(R.string.trust_and_forget) { _, _ ->
                RelayConnection.trustHost(hostClean)
                toast(R.string.host_trusted)
            }
            .show()
    }

    private fun toast(resId: Int) = Toast.makeText(this, resId, Toast.LENGTH_SHORT).show()

}
