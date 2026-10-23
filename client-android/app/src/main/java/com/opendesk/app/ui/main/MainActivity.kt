package com.opendesk.app.ui.main

import android.Manifest
import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.text.InputType
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import com.opendesk.app.R
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.platform.audio.VoiceCallManager
import com.opendesk.app.platform.capture.ScreenCaptureService
import com.opendesk.app.platform.input.OpenDeskAccessibilityService
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.viewer.RemoteViewerActivity

/** Home screen: this phone's ID, the relay connection, connecting to a PC and the hosted-session card. */
class MainActivity : AppCompatActivity(), RelayConnection.Listener {

    private lateinit var settings: SettingsRepository
    private lateinit var tvStatus: TextView
    private lateinit var tvGatewayUrl: TextView
    private lateinit var etTargetId: EditText
    private lateinit var btnConnect: Button
    private lateinit var btnAccessibilityStatus: Button
    private lateinit var btnOverlayStatus: Button
    private lateinit var cardActiveHostSession: View
    private lateinit var tvActiveHostPartner: TextView
    private lateinit var btnHostToggleMic: Button

    /** Screen-capture consent, asked before every hosted session. */
    private val captureConsent = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val data = result.data
        if (result.resultCode == Activity.RESULT_OK && data != null) {
            val manager = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
            ScreenCaptureService.mediaProjection = manager.getMediaProjection(result.resultCode, data)
            ScreenCaptureService.frameSink = RelayConnection::sendBinary
            startService(Intent(this, ScreenCaptureService::class.java))
            RelayConnection.respondToRequest(true)
        } else {
            RelayConnection.respondToRequest(false)
        }
    }

    private val micPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            VoiceCallManager.setMicMuted(false)
            updateHostMicButton()
        }
    }

    private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        settings = SettingsRepository(this)

        tvStatus = findViewById(R.id.tv_status)
        tvGatewayUrl = findViewById(R.id.tv_gateway_url)
        etTargetId = findViewById(R.id.et_target_id)
        btnConnect = findViewById(R.id.btn_connect)
        btnAccessibilityStatus = findViewById(R.id.btn_accessibility_status)
        btnOverlayStatus = findViewById(R.id.btn_overlay_status)
        cardActiveHostSession = findViewById(R.id.card_active_host_session)
        tvActiveHostPartner = findViewById(R.id.tv_active_host_partner)
        btnHostToggleMic = findViewById(R.id.btn_host_toggle_mic)

        val deskId = settings.deskId
        findViewById<TextView>(R.id.tv_my_id).text = deskId
        findViewById<Button>(R.id.btn_copy_id).setOnClickListener {
            val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            clipboard.setPrimaryClip(ClipData.newPlainText(getString(R.string.clip_label_id), deskId))
            toast(R.string.id_copied)
        }
        findViewById<Button>(R.id.btn_settings_gateway).setOnClickListener { showGatewaySettingsDialog() }
        findViewById<Button>(R.id.btn_stop_host_session).setOnClickListener { endHosting() }
        btnAccessibilityStatus.setOnClickListener { openAccessibilitySettings() }
        btnOverlayStatus.setOnClickListener { openOverlaySettings() }
        btnHostToggleMic.setOnClickListener { toggleHostMic() }
        btnConnect.setOnClickListener { connectToTarget() }

        requestNotificationPermission()
        promptForAccessibilityOnce()
        RelayConnection.listener = this
        connectRelay()
    }

    override fun onResume() {
        super.onResume()
        updatePermissionButtons()
    }

    override fun onDestroy() {
        RelayConnection.listener = null
        RelayConnection.stop()
        stopService(Intent(this, ScreenCaptureService::class.java))
        super.onDestroy()
    }

    // ---- Relay ----

    private fun connectRelay() {
        val url = settings.gatewayUrl
        tvGatewayUrl.text = getString(R.string.relay_label, url.ifEmpty { getString(R.string.relay_not_configured) })
        if (!RelayConnection.start(this, settings)) {
            onRelayStatus(false)
            showGatewaySettingsDialog()
        }
    }

    private fun connectToTarget() {
        val target = etTargetId.text.toString().trim()
        if (target.isEmpty()) {
            toast(R.string.enter_target_id)
            return
        }
        btnConnect.isEnabled = false
        btnConnect.setText(R.string.connecting)
        RelayConnection.connectTo(target)
    }

    private fun resetConnectButton() {
        btnConnect.isEnabled = true
        btnConnect.setText(R.string.connect_to_pc)
    }

    private fun showGatewaySettingsDialog() {
        val urlInput = EditText(this).apply {
            setText(settings.gatewayUrl)
            setSelection(text.length)
            hint = getString(R.string.relay_url_hint)
        }
        val tokenInput = EditText(this).apply {
            hint = getString(R.string.device_token_hint)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            setText(settings.deviceToken)
        }
        val fields = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val padding = resources.getDimensionPixelSize(R.dimen.dialog_padding)
            setPadding(padding, padding, padding, 0)
            addView(urlInput)
            addView(tokenInput)
        }
        AlertDialog.Builder(this)
            .setTitle(R.string.relay_settings_title)
            .setMessage(R.string.relay_settings_message)
            .setView(fields)
            .setPositiveButton(R.string.save_and_connect) { _, _ ->
                val url = urlInput.text.toString().trim()
                val token = tokenInput.text.toString().trim()
                // The token is optional (open relays need none); a provisioned one is at least 32 characters.
                if (url.isEmpty() || (token.isNotEmpty() && token.length < MIN_TOKEN_LENGTH)) {
                    toast(R.string.relay_settings_invalid)
                    return@setPositiveButton
                }
                settings.gatewayUrl = url
                settings.deviceToken = token
                connectRelay()
                toast(R.string.relay_updated)
            }
            .setNegativeButton(R.string.cancel, null)
            .show()
    }

    // ---- Hosting (a PC controls this phone) ----

    private fun toggleHostMic() {
        if (!VoiceCallManager.isMuted) {
            VoiceCallManager.setMicMuted(true)
            updateHostMicButton()
        } else if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            micPermission.launch(Manifest.permission.RECORD_AUDIO)
        } else {
            VoiceCallManager.setMicMuted(false)
            updateHostMicButton()
        }
    }

    private fun endHosting() {
        RelayConnection.endHosting()
        stopService(Intent(this, ScreenCaptureService::class.java))
        cardActiveHostSession.visibility = View.GONE
        updateHostMicButton()
        toast(R.string.remote_session_stopped)
    }

    private fun updateHostMicButton() {
        val muted = VoiceCallManager.isMuted
        btnHostToggleMic.setText(if (muted) R.string.mic_off else R.string.mic_on)
        btnHostToggleMic.setBackgroundColor(ContextCompat.getColor(this, if (muted) android.R.color.transparent else R.color.accent))
    }

    // ---- RelayConnection.Listener ----

    override fun onRelayStatus(online: Boolean) {
        tvStatus.setText(if (online) R.string.status_connected else R.string.status_disconnected)
        tvStatus.setTextColor(ContextCompat.getColor(this, if (online) R.color.status_online else R.color.status_offline))
    }

    override fun onIncomingRequest() {
        val manager = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        captureConsent.launch(manager.createScreenCaptureIntent())
    }

    override fun onViewerConnected(partnerClean: String) {
        resetConnectButton()
        startActivity(Intent(this, RemoteViewerActivity::class.java))
    }

    override fun onHostingStarted(partnerId: String) {
        resetConnectButton()
        tvActiveHostPartner.text = getString(R.string.connected_to, partnerId)
        cardActiveHostSession.visibility = View.VISIBLE
        updateHostMicButton()
    }

    override fun onSessionClosed(message: String?) {
        Toast.makeText(this, message ?: getString(R.string.session_closed), Toast.LENGTH_SHORT).show()
        cardActiveHostSession.visibility = View.GONE
        updateHostMicButton()
        stopService(Intent(this, ScreenCaptureService::class.java))
        RemoteViewerActivity.finishIfOpen()
    }

    override fun onError(message: String?) {
        resetConnectButton()
        Toast.makeText(this, message ?: getString(R.string.error_generic), Toast.LENGTH_SHORT).show()
    }

    override fun onMicMuted() = updateHostMicButton()

    override fun onNotice(text: String) {
        Toast.makeText(this, text, Toast.LENGTH_SHORT).show()
    }

    // ---- System permissions ----

    private fun isOverlayEnabled() = Build.VERSION.SDK_INT < Build.VERSION_CODES.M || Settings.canDrawOverlays(this)

    private fun updatePermissionButtons() {
        showPermissionState(btnAccessibilityStatus, OpenDeskAccessibilityService.isRunning)
        showPermissionState(btnOverlayStatus, isOverlayEnabled())
    }

    private fun showPermissionState(button: Button, granted: Boolean) {
        button.setText(if (granted) R.string.permission_active else R.string.permission_enable)
        button.setBackgroundColor(ContextCompat.getColor(this, if (granted) R.color.status_online else R.color.status_error))
        button.isEnabled = !granted
    }

    private fun promptForAccessibilityOnce() {
        if (OpenDeskAccessibilityService.isRunning || settings.promptedForAccessibility) return
        settings.promptedForAccessibility = true
        AlertDialog.Builder(this)
            .setTitle(R.string.welcome_title)
            .setMessage(R.string.welcome_message)
            .setPositiveButton(R.string.open_settings) { _, _ -> openAccessibilitySettings() }
            .setNegativeButton(R.string.later, null)
            .show()
    }

    private fun openAccessibilitySettings() {
        try {
            startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
            Toast.makeText(this, R.string.accessibility_hint, Toast.LENGTH_LONG).show()
        } catch (e: android.content.ActivityNotFoundException) {
            toast(R.string.accessibility_manual)
        }
    }

    private fun openOverlaySettings() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) return
        try {
            startActivity(Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION, Uri.parse("package:$packageName")))
        } catch (e: android.content.ActivityNotFoundException) {
            startActivity(Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION))
        }
    }

    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    private fun toast(resId: Int) = Toast.makeText(this, resId, Toast.LENGTH_SHORT).show()

    private companion object {
        const val MIN_TOKEN_LENGTH = 32
    }
}
