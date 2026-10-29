package com.opendesk.app.session

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.os.Handler
import android.os.Looper
import com.google.gson.Gson
import com.google.gson.JsonParser
import com.opendesk.app.R
import com.opendesk.app.data.dto.ApprovalResponse
import com.opendesk.app.data.dto.ConnectRequest
import com.opendesk.app.data.dto.InputEvent
import com.opendesk.app.data.dto.MessageType
import com.opendesk.app.data.dto.P2pSignal
import com.opendesk.app.data.dto.RegisterRequest
import com.opendesk.app.data.dto.RelayInput
import com.opendesk.app.data.dto.RemoteEvent
import com.opendesk.app.data.dto.ServerMessage
import com.opendesk.app.data.dto.SessionDisconnect
import com.opendesk.app.data.network.GatewayClient
import com.opendesk.app.data.security.DeviceKeys
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.Capabilities
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.KeyFingerprint
import com.opendesk.app.platform.audio.VoiceCallManager

/**
 * The app's single relay connection and its message routing. Activities observe it through [Listener] and
 * [ViewerListener].
 *
 * Android is a viewer only: at most one session, in which this phone controls a PC. It registers with the
 * `viewer-only` capability and declines every `incoming-request` a relay might still send. A dropped connection
 * is retried with exponential backoff, and at once when the network comes back.
 *
 * End to end: with a host that lists `e2e` (and a relay that forwards `p2p-signal`) the session runs an
 * [E2eChannel]. Input and voice wait until it is on, and a session whose handshake does not complete within
 * [E2E_TIMEOUT_MS] is closed. The host's key is pinned on first contact; a changed key, or a pinned host that no
 * longer offers `e2e`, closes the session and asks the user ([Listener.onSecurityAlert]).
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

        /**
         * The session with `hostClean` was closed for safety: its key changed, or it stopped offering end-to-end
         * encryption. The user may accept that ([trustHost]) and connect again.
         */
        fun onSecurityAlert(hostClean: String, message: String)
    }

    /** What the viewer screen needs. Main thread. */
    interface ViewerListener {
        /** The session is end-to-end encrypted now. */
        fun onEndToEnd() {}

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

    /** Whether the current session is end-to-end encrypted in both directions. */
    val isEndToEnd: Boolean
        get() = e2e?.isOn == true

    /** This phone's key fingerprint, to compare with what hosts log; null before [start]. */
    val keyFingerprint: String?
        get() = keys?.publicKey?.let(KeyFingerprint::of)

    private val mainHandler = Handler(Looper.getMainLooper())
    private val gson = Gson()
    private var client: GatewayClient? = null
    private var appContext: Context? = null
    private var settings: SettingsRepository? = null
    private var fileReceiver: FileReceiver? = null
    private var keys: DeviceKeys? = null
    private var relayCaps: List<String> = emptyList()

    /** The session's end-to-end channel; set on the main thread, read on every thread. */
    @Volatile
    private var e2e: E2eChannel? = null
    private val e2eTimeout = Runnable { onE2eTimeout() }
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
        if (keys == null) keys = DeviceKeys(app)
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

    /** Sends voice to the relay: sealed in an end-to-end session, and not at all while its handshake runs. */
    fun sendBinary(bytes: ByteArray) {
        val channel = e2e
        val out = when {
            channel == null -> bytes
            channel.isOn -> channel.seal(bytes) ?: return
            else -> return
        }
        client?.sendBinary(out)
    }

    /** Asks to view `targetId` (any formatting). Returns false while the relay is unreachable. */
    fun connectTo(targetId: String): Boolean {
        val c = client
        if (!online || c == null) return false
        c.send(ConnectRequest(targetId))
        return true
    }

    /**
     * Sends an input event to the viewed host. In an end-to-end session it is sealed; while the handshake runs only
     * `video-keyframe` (which says nothing about the user) goes out, so no touch or keystroke reaches the relay in
     * clear.
     */
    fun sendInput(event: InputEvent) {
        val partner = viewerPartner
        if (partner.isEmpty()) return
        val channel = e2e
        val payload: Any = when {
            channel == null -> event
            channel.isOn -> channel.sealEvent(gson.toJson(event)) ?: return
            event.action == VIDEO_KEYFRAME -> event
            else -> return
        }
        client?.send(RelayInput(partner, payload))
    }

    /** Accepts `hostClean`'s new key (or its lack of end-to-end encryption): the next session pins what it presents. */
    fun trustHost(hostClean: String) {
        keys?.forget(DeskId.clean(hostClean))
    }

    /** Ends the viewer session. */
    fun endViewing() {
        if (viewerPartner.isNotEmpty()) client?.send(SessionDisconnect(viewerPartner))
        viewerPartner = ""
        clearE2e()
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

    override fun onBinary(raw: ByteArray) {
        // In an end-to-end session: forged, replayed, or plain on a stream that is sealed now is dropped.
        val channel = e2e
        val bytes = if (channel == null) raw else channel.open(raw) ?: return
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
            MessageType.INPUT_COMMAND -> message.event?.let { raw ->
                val channel = e2e
                val event = if (channel == null) raw else channel.openEvent(raw) ?: return
                onControl(gson.fromJson(event, RemoteEvent::class.java))
            }
            MessageType.P2P_SIGNAL -> onSignal(message)
            MessageType.REGISTERED_ACK -> relayCaps = message.caps.orEmpty()
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
        if (!startE2e(partner, message.peerCaps.orEmpty())) return
        VoiceCallManager.startSession(partner, ::sendBinary)
        listener?.onViewerConnected(partner)
    }

    /**
     * Starts the end-to-end handshake with a host that offers it. False when the session was refused: a host with
     * a pinned key that no longer offers `e2e` (or a relay that no longer forwards the handshake) is treated like a
     * changed key, since a relay or an impostor holding the host's ID can strip the capability.
     */
    private fun startE2e(partner: String, peerCaps: List<String>): Boolean {
        clearE2e()
        val keys = keys ?: return true
        val privateKey = keys.privateKey
        val offered = Capabilities.E2E in peerCaps && Capabilities.RELAY_P2P_SIGNAL in relayCaps
        if (!offered || privateKey == null) {
            if (keys.pinned(partner) == null) return true
            refuse(partner, appContext?.getString(R.string.e2e_downgraded, DeskId.format(partner)))
            return false
        }
        val local = DeskId.clean(settings?.deskId ?: "")
        val (channel, first) = E2eChannel.start(false, local, partner, privateKey) ?: return true
        e2e = channel
        first?.let { client?.send(P2pSignal(partner, it)) }
        mainHandler.postDelayed(e2eTimeout, E2E_TIMEOUT_MS)
        return true
    }

    private fun onSignal(message: ServerMessage) {
        val channel = e2e ?: return
        val from = DeskId.clean(message.fromClean ?: "")
        if (from != channel.partner || from != viewerPartner) return
        val signal = runCatching { JsonParser.parseString(message.candidate ?: "").asJsonObject }.getOrNull() ?: return
        if (!E2eChannel.isSignal(signal)) return
        var changed: DeviceKeys.Pin.Mismatch? = null
        var presented: ByteArray? = null
        val reply = channel.onSignal(signal) { key ->
            presented = key
            when (val pin = keys?.check(from, key)) {
                is DeviceKeys.Pin.Mismatch -> {
                    changed = pin
                    false
                }
                else -> true
            }
        }
        reply?.let { client?.send(P2pSignal(from, it)) }
        val mismatch = changed
        when {
            mismatch != null -> refuse(
                from,
                appContext?.getString(
                    R.string.e2e_key_changed,
                    DeskId.format(from),
                    presented?.let(KeyFingerprint::of) ?: "?",
                    KeyFingerprint.of(mismatch.pinned),
                ),
            )
            channel.isOn -> {
                mainHandler.removeCallbacks(e2eTimeout)
                viewerListener?.onEndToEnd()
            }
            channel.hasFailed -> refuse(from, appContext?.getString(R.string.e2e_failed))
        }
    }

    private fun onE2eTimeout() {
        val channel = e2e ?: return
        if (!channel.isOn && channel.partner == viewerPartner) {
            refuse(channel.partner, appContext?.getString(R.string.e2e_failed))
        }
    }

    /** Ends the session with `partner` for safety and tells the user why. */
    private fun refuse(partner: String, message: String?) {
        client?.send(SessionDisconnect(partner))
        val text = message ?: return
        endSession(text, notify = true, fromRelay = false)
        listener?.onSecurityAlert(partner, text)
    }

    private fun clearE2e() {
        mainHandler.removeCallbacks(e2eTimeout)
        e2e = null
    }

    /** Clears the session; the main screen hears about it when there was one or the relay said so. */
    private fun endSession(message: String?, notify: Boolean, fromRelay: Boolean) {
        val hadSession = viewerPartner.isNotEmpty()
        viewerPartner = ""
        clearE2e()
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
    private const val VIDEO_KEYFRAME = "video-keyframe"
    private const val E2E_TIMEOUT_MS = 10_000L
    private const val RETRY_MIN_MS = 1_000L
    private const val RETRY_MAX_MS = 30_000L
}
