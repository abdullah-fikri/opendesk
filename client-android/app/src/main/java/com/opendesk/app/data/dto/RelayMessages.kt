package com.opendesk.app.data.dto

import com.google.gson.JsonObject
import com.google.gson.annotations.SerializedName

/*
 * Wire format between the Android client and the relay (see docs/protocol.md at the repository root).
 * Outbound messages are plain data classes serialized by Gson; null fields are omitted.
 * Android is a viewer only. It advertises `viewer-only`, `video-delta` and `e2e` but no multi-session
 * capability, so it receives framed JPEG video and the single-session message shapes. In administrator mode it
 * also registers with `isAdmin` and asks for the device registry (`get-hosts` / `host-list`).
 */

/** Message `type` values. */
object MessageType {
    const val REGISTER = "register"
    const val GET_HOSTS = "get-hosts"
    const val HOST_RENAME = "host-rename"
    const val CONNECT_REQUEST = "connect-request"
    const val APPROVAL_RESPONSE = "approval-response"
    const val SESSION_DISCONNECT = "session-disconnect"
    const val RELAY_INPUT = "relay-input"
    const val P2P_SIGNAL = "p2p-signal"

    const val REGISTERED_ACK = "registered-ack"
    const val HOST_LIST = "host-list"
    const val INCOMING_REQUEST = "incoming-request"
    const val SESSION_CONNECTED = "session-connected"
    const val INPUT_COMMAND = "input-command"
    const val FILE_TRANSFER = "file-transfer"
    const val SESSION_CLOSED = "session-closed"
    const val ERROR = "error"
    const val DEVICE_CREDENTIAL = "device-credential"
}

/** First message on every connection. */
data class RegisterRequest(
    val deskId: String,
    val deviceToken: String,
    val deviceName: String,
    val caps: List<String>,
    /** Administrator mode: the relay sends this phone the device registry (ignored when device security decides roles). */
    val isAdmin: Boolean = false,
    val platform: String = "android",
    val type: String = MessageType.REGISTER,
)

/** Asks for the device registry (answered with `host-list` to administrators only). */
data class GetHosts(val type: String = MessageType.GET_HOSTS)

/** Administrators: names a device in the relay's registry for every administrator (empty clears the name). */
data class HostRename(val targetClean: String, val alias: String, val type: String = MessageType.HOST_RENAME)

/** Opens a viewer session to `targetId`. */
data class ConnectRequest(val targetId: String, val type: String = MessageType.CONNECT_REQUEST)

/** Answer to an `incoming-request`. Android never hosts, so it only ever sends `accept = false`. */
data class ApprovalResponse(val fromClean: String, val accept: Boolean, val type: String = MessageType.APPROVAL_RESPONSE)

/** Ends the session with `targetClean`. */
data class SessionDisconnect(val targetClean: String, val type: String = MessageType.SESSION_DISCONNECT)

/** An input event for the viewed host: an [InputEvent], or its sealed `{"action":"e2e"}` form. */
data class RelayInput(val targetClean: String, val event: Any, val type: String = MessageType.RELAY_INPUT)

/** Carries the end-to-end handshake to the session partner (`candidate` is a JSON string). */
data class P2pSignal(val targetClean: String, val candidate: String, val type: String = MessageType.P2P_SIGNAL)

/**
 * Input or control event. Coordinates are normalized to 0..1 of the remote screen.
 * Actions sent by Android: mousemove, mousedown, mouseup, click, dblclick, wheel, keydown, keyup, text,
 * shortcut, switch-monitor, set-mic.
 */
data class InputEvent(
    val action: String,
    val normX: Float? = null,
    val normY: Float? = null,
    val button: Int? = null,
    val deltaY: Int? = null,
    val muted: Boolean? = null,
    /** Win32 virtual-key code for keydown/keyup. */
    val keyCode: Int? = null,
    /** Typed characters for `text`. */
    val text: String? = null,
    /** Shortcut name for `shortcut` (win, alt-tab, taskmgr, explorer, desktop). */
    val name: String? = null,
)

/** Any message from the relay. Every field is optional; `type` selects which ones are set. */
data class ServerMessage(
    val type: String? = null,
    val fromClean: String? = null,
    val targetId: String? = null,
    val role: String? = null,
    val message: String? = null,
    val event: JsonObject? = null,
    val payload: FilePayload? = null,
    /** `device-credential`: the token the relay issued when it enrolled this device. */
    val token: String? = null,
    /** `registered-ack`: the relay's capabilities. */
    val caps: List<String>? = null,
    /** `session-connected`: the partner's capabilities. */
    val peerCaps: List<String>? = null,
    /** `p2p-signal`: the partner's signal (JSON text). */
    val candidate: String? = null,
    /** `host-list`: every device the relay knows. */
    val hosts: List<HostInfo>? = null,
)

/** One device of a `host-list`. */
data class HostInfo(
    @SerializedName("clean_id") val cleanId: String? = null,
    @SerializedName("desk_id") val deskId: String? = null,
    @SerializedName("device_name") val deviceName: String? = null,
    /** The name an administrator gave the device; empty when none. */
    val alias: String? = null,
    val platform: String? = null,
    val online: Boolean = false,
)

/** Received control event (`input-command.event`). A viewer only acts on `set-mic`. */
data class RemoteEvent(
    val action: String? = null,
    val muted: Boolean? = null,
)

/** File-transfer payload as Android receives it (`start`, `chunk`, `end`). */
data class FilePayload(
    val action: String? = null,
    val id: String? = null,
    val name: String? = null,
    val size: Long? = null,
    val chunk: String? = null,
    val sha256: String? = null,
)
