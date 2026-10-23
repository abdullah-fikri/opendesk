package com.opendesk.app.session

import android.content.Context
import com.google.gson.Gson
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
import com.opendesk.app.domain.DeskId
import com.opendesk.app.platform.audio.VoiceCallManager

/**
 * The app's single relay connection and its message routing. Activities observe it through [Listener];
 * the viewer screen and the capture service plug into it for video.
 *
 * Android is a single-session client: at most one session as host (a PC controls this phone) or as viewer
 * (this phone controls a PC).
 */
object RelayConnection : GatewayClient.Listener {

    /** What the main screen shows. All calls arrive on the main thread. */
    interface Listener {
        /** The relay registered this phone (`true`) or the connection is gone (`false`). */
        fun onRelayStatus(online: Boolean)

        /** A PC asks to control this phone; answer with [respondToRequest] after asking for screen capture. */
        fun onIncomingRequest()

        /** This phone controls `partnerClean` now: open the viewer screen. */
        fun onViewerConnected(partnerClean: String)

        /** A PC controls this phone now; `partnerId` is its display ID. */
        fun onHostingStarted(partnerId: String)

        /** The session ended; `message` comes from the relay when it gave one. */
        fun onSessionClosed(message: String?)

        /** The relay refused a request. */
        fun onError(message: String?)

        /** The remote side muted this phone's microphone. */
        fun onMicMuted()

        /** A short notice for the user (file transfer progress and results). */
        fun onNotice(text: String)
    }

    /** Status listener; normally the main activity. */
    var listener: Listener? = null

    /** Receives every video frame while the viewer screen is open (socket thread). */
    @Volatile
    var videoSink: ((ByteArray) -> Unit)? = null

    /** Host this phone views, or empty. */
    @Volatile
    var viewerPartner: String = ""
        private set

    /** Viewer that controls this phone, or empty. */
    @Volatile
    var hostPartner: String = ""
        private set

    private var client: GatewayClient? = null
    private var pendingCaller: String? = null
    private var fileReceiver: FileReceiver? = null
    private var inputHandler: RemoteInputHandler? = null
    private val gson = Gson()

    /** (Re)connects with the stored settings. Returns false when no relay is configured. */
    fun start(context: Context, settings: SettingsRepository): Boolean {
        client?.disconnect()
        client = null
        fileReceiver?.close()
        fileReceiver = FileReceiver(context.applicationContext) { listener?.onNotice(it) }
        inputHandler = RemoteInputHandler(context.applicationContext)
        val url = settings.gatewayUrl
        if (url.isEmpty()) return false
        val registration = RegisterRequest(
            deskId = settings.deskId,
            deviceToken = settings.deviceToken,
            deviceName = "Android (${settings.deskId})",
        )
        client = GatewayClient(registration, this).also { it.connect(url) }
        return true
    }

    /** Closes the connection and every transfer (activity destroyed). */
    fun stop() {
        VoiceCallManager.stopSession()
        fileReceiver?.close()
        client?.disconnect()
        client = null
    }

    /** Sends voice or video to the relay. */
    fun sendBinary(bytes: ByteArray) {
        client?.sendBinary(bytes)
    }

    /** Asks to view `targetId` (any formatting). */
    fun connectTo(targetId: String) {
        client?.send(ConnectRequest(targetId))
    }

    /** Answers the pending incoming request. */
    fun respondToRequest(accept: Boolean) {
        val caller = pendingCaller ?: return
        pendingCaller = null
        client?.send(ApprovalResponse(caller, accept))
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

    /** Ends the hosted session. */
    fun endHosting() {
        if (hostPartner.isNotEmpty()) client?.send(SessionDisconnect(hostPartner))
        hostPartner = ""
        VoiceCallManager.stopSession()
    }

    // ---- GatewayClient.Listener ----

    override fun onConnectionChanged(online: Boolean) {
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
            // Android asks for screen-capture consent before every session it hosts.
            MessageType.INCOMING_REQUEST -> {
                pendingCaller = message.fromClean
                listener?.onIncomingRequest()
            }
            MessageType.SESSION_CONNECTED -> onSessionConnected(message)
            MessageType.FILE_TRANSFER -> message.payload?.let { fileReceiver?.handle(it) }
            MessageType.INPUT_COMMAND -> message.event?.let { onInput(gson.fromJson(it, RemoteEvent::class.java)) }
            MessageType.SESSION_CLOSED -> {
                viewerPartner = ""
                hostPartner = ""
                VoiceCallManager.stopSession()
                listener?.onSessionClosed(message.message)
            }
            MessageType.ERROR -> listener?.onError(message.message)
        }
    }

    private fun onSessionConnected(message: ServerMessage) {
        val partnerId = message.targetId ?: ""
        val partner = DeskId.clean(partnerId)
        val sink: (ByteArray) -> Unit = ::sendBinary
        when (message.role) {
            "viewer" -> {
                viewerPartner = partner
                VoiceCallManager.startSession(partner, sink)
                listener?.onViewerConnected(partner)
            }
            "host" -> {
                hostPartner = partner
                VoiceCallManager.startSession(partner, sink)
                listener?.onHostingStarted(partnerId)
            }
        }
    }

    private fun onInput(event: RemoteEvent) {
        // A remote peer may silence this microphone, but only the device owner can enable it.
        if (event.action == "set-mic") {
            if (event.muted == true) {
                VoiceCallManager.setMicMuted(true)
                listener?.onMicMuted()
            }
            return
        }
        if (hostPartner.isNotEmpty()) inputHandler?.apply(event)
    }

    private const val VOICE_MARKER: Byte = 0x02
}
