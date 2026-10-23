package com.opendesk.app

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
import android.os.Environment
import android.provider.Settings
import android.util.Base64
import android.view.LayoutInflater
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.google.gson.JsonObject
import java.io.File
import java.io.FileOutputStream
import java.security.MessageDigest

class MainActivity : AppCompatActivity() {

    private lateinit var tvStatus: TextView
    private lateinit var tvGatewayUrl: TextView
    private lateinit var tvMyId: TextView
    private lateinit var etTargetId: EditText
    private lateinit var btnConnect: Button
    private lateinit var btnCopyId: Button
    private lateinit var btnSettingsGateway: Button
    private lateinit var btnAccessibilityStatus: Button
    private lateinit var btnOverlayStatus: Button
    private lateinit var cardActiveHostSession: android.view.View
    private lateinit var tvActiveHostPartner: TextView
    private lateinit var btnHostToggleMic: Button
    private lateinit var btnStopHostSession: Button

    private lateinit var myDeskId: String
    private var gatewayClient: GatewayClient? = null
    private var pendingCallerClean: String? = null
    private var activeHostPartnerClean: String = ""

    private val CAPTURE_REQ_CODE = 1001
    private val DEFAULT_GATEWAY = "wss://contacted-fully-victorian-cdna.trycloudflare.com"

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        tvStatus = findViewById(R.id.tv_status)
        tvGatewayUrl = findViewById(R.id.tv_gateway_url)
        tvMyId = findViewById(R.id.tv_my_id)
        etTargetId = findViewById(R.id.et_target_id)
        btnConnect = findViewById(R.id.btn_connect)
        btnCopyId = findViewById(R.id.btn_copy_id)
        btnSettingsGateway = findViewById(R.id.btn_settings_gateway)
        btnAccessibilityStatus = findViewById(R.id.btn_accessibility_status)
        btnOverlayStatus = findViewById(R.id.btn_overlay_status)
        cardActiveHostSession = findViewById(R.id.card_active_host_session)
        tvActiveHostPartner = findViewById(R.id.tv_active_host_partner)
        btnHostToggleMic = findViewById(R.id.btn_host_toggle_mic)
        btnStopHostSession = findViewById(R.id.btn_stop_host_session)

        btnHostToggleMic.setOnClickListener {
            if (VoiceCallManager.isMuted) {
                if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                    ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.RECORD_AUDIO), 206)
                } else {
                    VoiceCallManager.setMicMuted(false, gatewayClient)
                    updateHostMicButtonUI()
                }
            } else {
                VoiceCallManager.setMicMuted(true, gatewayClient)
                updateHostMicButtonUI()
            }
        }

        btnStopHostSession.setOnClickListener {
            VoiceCallManager.stopSession()
            updateHostMicButtonUI()
            stopService(Intent(this, ScreenCaptureService::class.java))
            if (activeHostPartnerClean.isNotEmpty()) {
                val disc = JsonObject().apply {
                    addProperty("type", "session-disconnect")
                    addProperty("targetClean", activeHostPartnerClean)
                }
                gatewayClient?.send(disc)
                activeHostPartnerClean = ""
            }
            cardActiveHostSession.visibility = android.view.View.GONE
            Toast.makeText(this, "Sesi remote dihentikan", Toast.LENGTH_SHORT).show()
        }

        myDeskId = DeviceIdHelper.getDeskId(this)
        tvMyId.text = myDeskId

        btnCopyId.setOnClickListener {
            val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            cm.setPrimaryClip(ClipData.newPlainText("OpenDesk ID", myDeskId))
            Toast.makeText(this, "ID disalin ke clipboard", Toast.LENGTH_SHORT).show()
        }

        btnSettingsGateway.setOnClickListener {
            showGatewaySettingsDialog()
        }

        btnAccessibilityStatus.setOnClickListener {
            openAccessibilitySettings()
        }

        btnOverlayStatus.setOnClickListener {
            openOverlaySettings()
        }

        btnConnect.setOnClickListener {
            val target = etTargetId.text.toString().trim()
            if (target.isEmpty()) {
                Toast.makeText(this, "Masukkan ID target", Toast.LENGTH_SHORT).show()
                return@setOnClickListener
            }
            btnConnect.isEnabled = false
            btnConnect.text = "Menghubungkan..."

            val req = JsonObject().apply {
                addProperty("type", "connect-request")
                addProperty("targetId", target)
            }
            gatewayClient?.send(req)
        }

        requestNotificationPermission()
        checkAndPromptInitialPermissions()
        initGateway()
    }

    override fun onResume() {
        super.onResume()
        updatePermissionButtonsUI()
    }

    // Permission Checking
    private fun isAccessibilityEnabled(): Boolean {
        return OpenDeskAccessibilityService.isRunning
    }

    private fun isOverlayEnabled(): Boolean {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            Settings.canDrawOverlays(this)
        } else {
            true
        }
    }

    private fun updatePermissionButtonsUI() {
        if (isAccessibilityEnabled()) {
            btnAccessibilityStatus.text = "✓ Aktif"
            btnAccessibilityStatus.setBackgroundColor(0xFF22C55E.toInt())
            btnAccessibilityStatus.isEnabled = false
        } else {
            btnAccessibilityStatus.text = "Aktifkan"
            btnAccessibilityStatus.setBackgroundColor(0xFFEF4444.toInt())
            btnAccessibilityStatus.isEnabled = true
        }

        if (isOverlayEnabled()) {
            btnOverlayStatus.text = "✓ Aktif"
            btnOverlayStatus.setBackgroundColor(0xFF22C55E.toInt())
            btnOverlayStatus.isEnabled = false
        } else {
            btnOverlayStatus.text = "Aktifkan"
            btnOverlayStatus.setBackgroundColor(0xFFEF4444.toInt())
            btnOverlayStatus.isEnabled = true
        }
    }

    private fun checkAndPromptInitialPermissions() {
        val prefs = getSharedPreferences("opendesk_prefs", Context.MODE_PRIVATE)
        val hasPrompted = prefs.getBoolean("has_prompted_accessibility", false)

        if (!isAccessibilityEnabled() && !hasPrompted) {
            prefs.edit().putBoolean("has_prompted_accessibility", true).apply()
            AlertDialog.Builder(this)
                .setTitle("Selamat Datang di OpenDesk")
                .setMessage("Agar laptop dapat mengontrol HP ini dari jarak jauh, silakan aktifkan Layanan Aksesibilitas 'OpenDesk'.\n\nSetelah klik Lanjut, pilih 'Aplikasi Terinstal / Downloaded apps' lalu aktifkan OpenDesk.")
                .setPositiveButton("Buka Pengaturan") { _, _ ->
                    openAccessibilitySettings()
                }
                .setNegativeButton("Nanti Saja", null)
                .show()
        }
    }

    private fun openAccessibilitySettings() {
        try {
            val intent = Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)
            startActivity(intent)
            Toast.makeText(this, "Cari 'OpenDesk' di daftar layanan & aktifkan", Toast.LENGTH_LONG).show()
        } catch (e: Exception) {
            Toast.makeText(this, "Buka Pengaturan > Aksesibilitas manual", Toast.LENGTH_SHORT).show()
        }
    }

    private fun openOverlaySettings() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            try {
                val intent = Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION, Uri.parse("package:$packageName"))
                startActivity(intent)
            } catch (e: Exception) {
                startActivity(Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION))
            }
        }
    }

    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), 102)
            }
        }
    }

    // Gateway Management
    private fun getActiveGatewayUrl(): String {
        val prefs = getSharedPreferences("opendesk_prefs", Context.MODE_PRIVATE)
        return prefs.getString("gateway_url", DEFAULT_GATEWAY) ?: DEFAULT_GATEWAY
    }

    private fun initGateway() {
        val gatewayUrl = getActiveGatewayUrl()
        tvGatewayUrl.text = "Gateway: $gatewayUrl"

        gatewayClient?.disconnect()
        gatewayClient = GatewayClient(
            deskId = myDeskId,
            deviceToken = getSharedPreferences("opendesk_prefs", Context.MODE_PRIVATE).getString("device_token", "") ?: "",
            onMessageListener = { msg -> handleGatewayMessage(msg) },
            onStatusListener = { online ->
                tvStatus.text = if (online) "Terhubung" else "Terputus"
                tvStatus.setTextColor(if (online) 0xFF22C55E.toInt() else 0xFF64748B.toInt())
            }
        )
        gatewayClient?.connect(gatewayUrl)
    }

    private fun showGatewaySettingsDialog() {
        val currentUrl = getActiveGatewayUrl()
        val input = EditText(this).apply {
            setText(currentUrl)
            setSelection(currentUrl.length)
            setPadding(40, 30, 40, 30)
        }

        val tokenInput = EditText(this).apply {
            hint = "Token perangkat (minimal 32 karakter)"
            inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD
            setText(getSharedPreferences("opendesk_prefs", Context.MODE_PRIVATE).getString("device_token", ""))
        }
        val fields = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(input)
            addView(tokenInput)
        }
        AlertDialog.Builder(this)
            .setTitle("Pengaturan Relay Gateway")
            .setMessage("Masukkan alamat WebSocket Relay Gateway dan token perangkat yang terdaftar di relay:")
            .setView(fields)
            .setPositiveButton("Simpan & Sambungkan") { _, _ ->
                val newUrl = input.text.toString().trim()
                val token = tokenInput.text.toString().trim()
                if (newUrl.isNotEmpty() && token.length >= 32) {
                    getSharedPreferences("opendesk_prefs", Context.MODE_PRIVATE)
                        .edit()
                        .putString("gateway_url", newUrl)
                        .putString("device_token", token)
                        .apply()
                    initGateway()
                    Toast.makeText(this, "Gateway diperbarui", Toast.LENGTH_SHORT).show()
                } else {
                    Toast.makeText(this, "Alamat dan token minimal 32 karakter wajib diisi", Toast.LENGTH_LONG).show()
                }
            }
            .setNeutralButton("Reset Default") { _, _ ->
                getSharedPreferences("opendesk_prefs", Context.MODE_PRIVATE)
                    .edit()
                    .putString("gateway_url", DEFAULT_GATEWAY)
                    .apply()
                initGateway()
                Toast.makeText(this, "Gateway di-reset ke default", Toast.LENGTH_SHORT).show()
            }
            .setNegativeButton("Batal", null)
            .show()
    }

    // Message Protocol
    private fun handleGatewayMessage(msg: JsonObject) {
        val type = msg.get("type")?.asString ?: return

        when (type) {
            "incoming-request" -> {
                pendingCallerClean = msg.get("fromClean")?.asString
                // Auto accept langsung tanpa dialog manual
                requestScreenCapturePermission()
            }

            "session-connected" -> {
                btnConnect.isEnabled = true
                btnConnect.text = "Hubungkan ke PC"
                val role = msg.get("role")?.asString
                val targetId = msg.get("targetId")?.asString ?: ""
                val cleanTarget = targetId.replace("\\D".toRegex(), "")

                if (role == "viewer") {
                    // Open Remote Viewer screen to see PC desktop
                    RemoteViewerActivity.gatewayClient = gatewayClient
                    RemoteViewerActivity.targetDeskClean = cleanTarget
                    VoiceCallManager.targetClean = cleanTarget
                    startActivity(Intent(this, RemoteViewerActivity::class.java))
                } else if (role == "host") {
                    activeHostPartnerClean = cleanTarget
                    tvActiveHostPartner.text = "Terhubung: $targetId"
                    cardActiveHostSession.visibility = android.view.View.VISIBLE
                    VoiceCallManager.startSession(gatewayClient, cleanTarget)
                    updateHostMicButtonUI()
                }
            }

            "frame" -> {
                val data = msg.get("data")?.asString ?: return
                RemoteViewerActivity.instance?.enqueueBase64Frame(data)
            }

            "file-transfer" -> {
                val payload = msg.getAsJsonObject("payload") ?: return
                handleIncomingFile(payload)
            }

            "input-command" -> {
                val event = msg.getAsJsonObject("event") ?: return
                handleRemoteInput(event)
            }

            "session-closed" -> {
                val message = msg.get("message")?.asString ?: "Sesi ditutup"
                Toast.makeText(this, message, Toast.LENGTH_SHORT).show()
                VoiceCallManager.stopSession()
                updateHostMicButtonUI()
                cardActiveHostSession.visibility = android.view.View.GONE
                stopService(Intent(this, ScreenCaptureService::class.java))
                RemoteViewerActivity.instance?.finish()
            }

            "error" -> {
                btnConnect.isEnabled = true
                btnConnect.text = "Hubungkan ke PC"
                val err = msg.get("message")?.asString ?: "Kesalahan"
                Toast.makeText(this, err, Toast.LENGTH_SHORT).show()
            }

            "waiting-approval" -> {
                Toast.makeText(this, "Menunggu persetujuan di layar target...", Toast.LENGTH_SHORT).show()
            }
        }
    }

    private fun handleRemoteInput(event: JsonObject) {
        val action = event.get("action")?.asString ?: return
        if (action == "audio") {
            val b64 = event.get("data")?.asString ?: return
            try {
                val pcm = Base64.decode(b64, Base64.DEFAULT)
                VoiceCallManager.playPcmChunk(pcm)
            } catch (_: Exception) {}
            return
        }
        if (action == "set-mic") {
            // Remote controls may silence capture, but only the device owner can enable it.
            if (event.get("muted")?.asBoolean == true) {
                VoiceCallManager.setMicMuted(true, gatewayClient)
                updateHostMicButtonUI()
            }
            return
        }
        val service = OpenDeskAccessibilityService.instance ?: return

        val dm = resources.displayMetrics
        val screenW = dm.widthPixels.toFloat()
        val screenH = dm.heightPixels.toFloat()

        when (action) {
            "click", "mouseup" -> {
                val normX = event.get("normX")?.asFloat ?: return
                val normY = event.get("normY")?.asFloat ?: return
                service.dispatchClick(normX * screenW, normY * screenH)
            }
            "key", "keydown" -> {
                val code = (event.get("code") ?: event.get("keyCode"))?.asInt ?: return
                if (code == 27) service.triggerBack()
                else if (code == 91) service.triggerHome()
            }
        }
    }

    private fun requestScreenCapturePermission() {
        val mgr = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        startActivityForResult(mgr.createScreenCaptureIntent(), CAPTURE_REQ_CODE)
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == CAPTURE_REQ_CODE) {
            if (resultCode == Activity.RESULT_OK && data != null) {
                val mgr = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                ScreenCaptureService.mediaProjection = mgr.getMediaProjection(resultCode, data)
                ScreenCaptureService.targetClean = pendingCallerClean
                ScreenCaptureService.gatewayClient = gatewayClient

                startService(Intent(this, ScreenCaptureService::class.java))
                respondApproval(true)
            } else {
                respondApproval(false)
            }
        }
    }

    private fun respondApproval(accept: Boolean) {
        val caller = pendingCallerClean ?: return
        val res = JsonObject().apply {
            addProperty("type", "approval-response")
            addProperty("fromClean", caller)
            addProperty("accept", accept)
        }
        gatewayClient?.send(res)
        pendingCallerClean = null
    }

    private val incomingFiles = mutableMapOf<String, FileOutputStream>()
    private val incomingFilePaths = mutableMapOf<String, File>()
    private val incomingFileNames = mutableMapOf<String, String>()
    private val incomingFileSizes = mutableMapOf<String, Long>()
    private val incomingFileReceived = mutableMapOf<String, Long>()
    private val incomingFileHashes = mutableMapOf<String, MessageDigest>()

    private fun handleIncomingFile(payload: JsonObject) {
        val action = payload.get("action")?.asString ?: return
        val id = payload.get("id")?.asString ?: return
        if (id.length !in 1..80 || !id.all { it.isLetterOrDigit() && it.code < 128 || it == '_' }) return

        when (action) {
            "start" -> {
                val rawName = payload.get("name")?.asString ?: "opendesk_${System.currentTimeMillis()}.bin"
                val cleanName = rawName.replace("[\\\\/:*?\"<>|]".toRegex(), "_")
                val size = payload.get("size")?.asLong ?: return
                if (incomingFiles.size >= 8 || incomingFiles.containsKey(id) || size !in 0..2147483648L || cleanName.isBlank() || cleanName.length > 200 || cleanName == "." || cleanName == ".." || cleanName.any { Character.isISOControl(it) }) return
                val downloadsDir = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)
                if (!downloadsDir.exists()) downloadsDir.mkdirs()
                var targetFile = File(downloadsDir, cleanName)
                try {
                    val staged = File.createTempFile(".opendesk-", ".part", downloadsDir)
                    val fos = FileOutputStream(staged)
                    targetFile = staged
                    incomingFiles[id] = fos
                    incomingFilePaths[id] = targetFile
                    incomingFileNames[id] = cleanName
                    incomingFileSizes[id] = size
                    incomingFileReceived[id] = 0
                    incomingFileHashes[id] = MessageDigest.getInstance("SHA-256")
                    Toast.makeText(this, "Menerima berkas: $cleanName...", Toast.LENGTH_SHORT).show()
                } catch (e: Exception) {
                    val fallbackDir = getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: filesDir
                    try {
                        targetFile = File.createTempFile(".opendesk-", ".part", fallbackDir)
                        val fos = FileOutputStream(targetFile)
                        incomingFiles[id] = fos
                        incomingFilePaths[id] = targetFile
                        incomingFileNames[id] = cleanName
                        incomingFileSizes[id] = size
                        incomingFileReceived[id] = 0
                        incomingFileHashes[id] = MessageDigest.getInstance("SHA-256")
                        Toast.makeText(this, "Menerima berkas: $cleanName...", Toast.LENGTH_SHORT).show()
                    } catch (e2: Exception) {}
                }
            }
            "chunk" -> {
                val b64 = payload.get("chunk")?.asString ?: return
                if (b64.length > 48000) return
                val bytes = try { Base64.decode(b64, Base64.DEFAULT) } catch (_: Exception) { return }
                val received = incomingFileReceived[id] ?: return
                val size = incomingFileSizes[id] ?: return
                if (bytes.size > 32768 || received + bytes.size > size) return
                try {
                    incomingFiles[id]?.write(bytes)
                    incomingFileHashes[id]?.update(bytes)
                    incomingFileReceived[id] = received + bytes.size
                } catch (_: Exception) {
                    incomingFiles.remove(id)?.runCatching { close() }
                    incomingFilePaths.remove(id)?.delete()
                    incomingFileNames.remove(id)
                    incomingFileHashes.remove(id)
                    incomingFileReceived.remove(id)
                    incomingFileSizes.remove(id)
                }
            }
            "end" -> {
                val staged = incomingFilePaths.remove(id) ?: return
                val expected = payload.get("sha256")?.asString
                val actual = incomingFileHashes.remove(id)?.digest()?.joinToString("") { "%02x".format(it) }
                val size = incomingFileSizes.remove(id)
                val received = incomingFileReceived.remove(id)
                val valid = expected != null && expected == actual && size == received
                val fileName = incomingFileNames.remove(id)
                val saved = File(staged.parentFile, fileName ?: ".")
                try {
                    val output = incomingFiles.remove(id) ?: return
                    output.flush()
                    output.fd.sync()
                    output.close()
                    if (!valid || fileName == null || !staged.renameTo(saved.takeIf { !it.exists() } ?: throw IllegalStateException("Nama file sudah dipakai"))) {
                        throw IllegalStateException("SHA-256, ukuran, atau nama file tidak valid")
                    }
                    Toast.makeText(this, "Berkas tersimpan: ${saved.name}", Toast.LENGTH_LONG).show()
                } catch (_: Exception) {
                    staged.delete()
                    Toast.makeText(this, "Gagal menerima berkas", Toast.LENGTH_LONG).show()
                }
            }
        }
    }

    private fun updateHostMicButtonUI() {
        if (VoiceCallManager.isMuted) {
            btnHostToggleMic.text = "Mic: Mati"
            btnHostToggleMic.setBackgroundColor(0x00000000)
        } else {
            btnHostToggleMic.text = "Mic: Aktif"
            btnHostToggleMic.setBackgroundColor(0xFFDC2626.toInt())
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == 206 && grantResults.isNotEmpty() && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            VoiceCallManager.setMicMuted(false, gatewayClient)
            updateHostMicButtonUI()
        }
    }

    override fun onDestroy() {
        incomingFiles.values.forEach { it.runCatching { close() } }
        incomingFilePaths.values.forEach { it.delete() }
        VoiceCallManager.stopSession()
        gatewayClient?.disconnect()
        super.onDestroy()
    }
}
