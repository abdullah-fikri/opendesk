package com.opendesk.app.session

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.os.Handler
import android.os.Looper
import com.google.gson.Gson
import com.opendesk.app.R
import com.opendesk.app.data.dto.ApprovalResponse
import com.opendesk.app.data.dto.ConnectRequest
import com.opendesk.app.data.dto.InputEvent
import com.opendesk.app.data.dto.MessageType
import com.opendesk.app.data.dto.RegisterRequest
import com.opendesk.app.data.dto.RelayInput
import com.opendesk.app.data.dto.RemoteEvent
import com.opendesk.app.data.dto.ServerMessage
import com.opendesk.app.data.dto.SessionDisconnect
import com.opendesk.app.data.network.GatewayClient
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.Capabilities
import com.opendesk.app.domain.DeskId
import com.opendesk.app.platform.audio.VoiceCallManager

/**
 * The app's single relay connection and its message routing. Activities observe it through [Listener] and
 * [ViewerListener].
 *
 * Android is a viewer only: at most one session, in which this phone controls a PC. It registers with the
 * `viewer-only` capability and declines every `incoming-request` a relay might still send. A dropped connection
 * is retried with exponential backoff, and at once when the network comes back.
 */
object RelayConnection : GatewayClient.Listener {

    /** What the main screen shows. All calls arrive on the main thread. */
    interface Listener {
        /** The relay registered this phone (`true`) or the connection is gone and being retried (`false`). */
        fun onRelayStatus(online: Boolean)

        /** This phone controls `partnerClean` now: open the viewer screen. */
        fun onViewerConnected(partnerClean: String)

        /** The session ended; `message` comes from the relay when it gave one. */
        fun onSessionClosed(message: String?)

        /** The relay refused a request. */
        fun onError(message: String?)

        /** A short notice for the user (file transfer progress and results). */
        fun onNotice(text: String)
    }

    /** What the viewer screen needs. Main thread. */
    interface ViewerListener {
        /** The session is over (closed by either side, or the relay connection dropped). */
        fun onViewerSessionEnded(message: String?)

        /** The remote side muted this phone's microphone. */
        fun onMicMuted()
    }

    /** Status listener; normally the main activity. */
    var listener: Listener? = null

    /** The open viewer screen, if any. */
    var viewerListener: ViewerListener? = null

    /** Receives every video frame while the viewer screen is open (socket thread). */
    @Volatile
    var videoSink: ((ByteArray) -> Unit)? = null

    /** Host this phone views, or empty. */
    @Volatile
    var viewerPartner: String = ""
        private set

    /** Whether [start] ran and [stop] did not since: the connection is up or being retried. */
    val isStarted: Boolean
        get() = settings != null

    /** Whether the relay has registered this phone. */
    var online: Boolean = false
        private set

    private val mainHandler = Handler(Looper.getMainLooper())
    private val gson = Gson()
    private var client: GatewayClient? = null
    private var appContext: Context? = null
    private var settings: SettingsRepository? = null
    private var fileReceiver: FileReceiver? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    private var retryDelayMs = RETRY_MIN_MS

    /** A connection attempt is in flight (between [reconnect] and its first status report). */
    private var connecting = false
    private val retry = Runnable { reconnect() }

    /** (Re)connects with the stored settings. Returns false when no relay is configured. */
    fun start(context: Context, settings: SettingsRepository): Boolean {
        val app = context.applicationContext
        appContext = app
        this.settings = settings
        if (fileReceiver == null) fileReceiver = FileReceiver(app) { listener?.onNotice(it) }
        registerNetworkCallback(app)
        retryDelayMs = RETRY_MIN_MS
        return reconnect()
    }

    /** Closes the connection, every transfer and the retry machinery (the app is leaving). */
    fun stop() {
        mainHandler.removeCallbacks(retry)
        unregisterNetworkCallback()
        endSession(null, notify = false, fromRelay = false)
        fileReceiver?.close()
        fileReceiver = null
        client?.disconnect()
        client = null
        online = false
        connecting = false
        settings = null
        appContext = null
    }

    /** Reconnects now if the connection is down (for example when the app comes back to the foreground). */
    fun retryNowIfOffline() {
        if (!online && !connecting && settings != null) {
            retryDelayMs = RETRY_MIN_MS
            reconnect()
        }
    }

    /** Sends voice to the relay. */
    fun sendBinary(bytes: ByteArray) {
        client?.sendBinary(bytes)
    }

    /** Asks to view `targetId` (any formatting). Returns false while the relay is unreachable. */
    fun connectTo(targetId: String): Boolean {
        val c = client
        if (!online || c == null) return false
        c.send(ConnectRequest(targetId))
        return true
    }

    /** Sends an input event to the viewed host. */
    fun sendInput(event: InputEvent) {
        if (viewerPartner.isNotEmpty()) client?.send(RelayInput(viewerPartner, event))
    }

    /** Ends the viewer session. */
    fun endViewing() {
        if (viewerPartner.isNotEmpty()) client?.send(SessionDisconnect(viewerPartner))
        viewerPartner = ""
        VoiceCallManager.stopSession()
    }

    // ---- GatewayClient.Listener ----

    override fun onConnectionChanged(online: Boolean) {
        this.online = online
        connecting = false
        if (online) {
            retryDelayMs = RETRY_MIN_MS
        } else {
            // The relay ends every session of a dropped connection and file grants are bound to it.
            endSession(appContext?.getString(R.string.connection_lost), notify = true, fromRelay = false)
            fileReceiver?.close()
            scheduleRetry()
        }
        listener?.onRelayStatus(online)
    }

    override fun onBinary(bytes: ByteArray) {
        if (bytes.size > 1 && bytes[0] == VOICE_MARKER) {
            VoiceCallManager.playPacket(bytes)
        } else {
            videoSink?.invoke(bytes)
        }
    }

    override fun onMessage(message: ServerMessage) {
        when (message.type) {
            // Only a relay that ignores `viewer-only` sends this; this phone can never be controlled.
            MessageType.INCOMING_REQUEST -> message.fromClean?.let { client?.send(ApprovalResponse(it, accept = false)) }
            MessageType.SESSION_CONNECTED -> onSessionConnected(message)
            MessageType.FILE_TRANSFER -> message.payload?.let { fileReceiver?.handle(it) }
            MessageType.INPUT_COMMAND -> message.event?.let { onControl(gson.fromJson(it, RemoteEvent::class.java)) }
            MessageType.SESSION_CLOSED -> endSession(message.message, notify = true, fromRelay = true)
            MessageType.ERROR -> listener?.onError(message.message)
            // The relay enrolled this phone: zero configuration besides the relay address.
            MessageType.DEVICE_CREDENTIAL -> message.token?.takeIf { it.isNotEmpty() }?.let { settings?.storeIssuedToken(it) }
        }
    }

    private fun onSessionConnected(message: ServerMessage) {
        val partner = DeskId.clean(message.targetId ?: "")
        if (message.role != "viewer" || partner.isEmpty()) {
            // Never act as a host: end anything else the relay may have paired.
            if (partner.isNotEmpty()) client?.send(SessionDisconnect(partner))
            return
        }
        viewerPartner = partner
        VoiceCallManager.startSession(partner, ::sendBinary)
        listener?.onViewerConnected(partner)
    }

    /** Clears the session; the main screen hears about it when there was one or the relay said so. */
    private fun endSession(message: String?, notify: Boolean, fromRelay: Boolean) {
        val hadSession = viewerPartner.isNotEmpty()
        viewerPartner = ""
        VoiceCallManager.stopSession()
        if (!notify) return
        viewerListener?.onViewerSessionEnded(message)
        if (hadSession || fromRelay) listener?.onSessionClosed(message)
    }

    private fun onControl(event: RemoteEvent) {
        // A remote peer may silence this microphone, but only the device owner can enable it.
        if (event.action == "set-mic" && event.muted == true) {
            VoiceCallManager.setMicMuted(true)
            viewerListener?.onMicMuted()
        }
    }

    // ---- Reconnection ----

    private fun reconnect(): Boolean {
        mainHandler.removeCallbacks(retry)
        val settings = settings ?: return false
        client?.disconnect()
        client = null
        online = false
        connecting = false
        val url = settings.gatewayUrl
        if (url.isEmpty()) return false
        connecting = true
        val registration = RegisterRequest(
            deskId = settings.deskId,
            deviceToken = settings.tokenForGateway,
            deviceName = "Android (${settings.deskId})",
            caps = Capabilities.advertised,
        )
        client = GatewayClient(registration, this).also { it.connect(url) }
        return true
    }

    private fun scheduleRetry() {
        if (settings == null) return
        mainHandler.removeCallbacks(retry)
        mainHandler.postDelayed(retry, retryDelayMs)
        retryDelayMs = (retryDelayMs * 2).coerceAtMost(RETRY_MAX_MS)
    }

    private fun registerNetworkCallback(context: Context) {
        if (networkCallback != null) return
        val manager = context.getSystemService(ConnectivityManager::class.java) ?: return
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                mainHandler.post { retryNowIfOffline() }
            }
        }
        runCatching { manager.registerDefaultNetworkCallback(callback) }.onSuccess { networkCallback = callback }
    }

    private fun unregisterNetworkCallback() {
        val callback = networkCallback ?: return
        networkCallback = null
        val manager = appContext?.getSystemService(ConnectivityManager::class.java) ?: return
        runCatching { manager.unregisterNetworkCallback(callback) }
    }

    private const val VOICE_MARKER: Byte = 0x02
    private const val RETRY_MIN_MS = 1_000L
    private const val RETRY_MAX_MS = 30_000L
}
