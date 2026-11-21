package com.opendesk.app.data.dto

import com.google.gson.JsonObject
import com.google.gson.annotations.SerializedName

/*
 * Wire format between the Android client and the relay (see docs/protocol.md at the repository root).
 * Outbound messages are plain data classes serialized by Gson; null fields are omitted.
 * Android is a viewer only. It advertises `viewer-only`, `video-delta` and `e2e` but no multi-session
 * capability, so it receives framed JPEG video and the single-session message shapes. The admin edition
 * also registers with `isAdmin`, asks for the device registry (`get-hosts` / `host-list`) and runs the monitor wall
 * (`monitor-view`: `monitor-set` / `monitor-state` and the `0x11` thumbnails).
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
    const val MONITOR_SET = "monitor-set"

    const val REGISTERED_ACK = "registered-ack"
    const val HOST_LIST = "host-list"
    const val INCOMING_REQUEST = "incoming-request"
    const val SESSION_CONNECTED = "session-connected"
    const val INPUT_COMMAND = "input-command"
    const val FILE_TRANSFER = "file-transfer"
    const val SESSION_CLOSED = "session-closed"
    const val ERROR = "error"
    const val DEVICE_CREDENTIAL = "device-credential"
    const val MONITOR_STATE = "monitor-state"
}

/** First message on every connection. */
data class RegisterRequest(
    val deskId: String,
    val deviceToken: String,
    val deviceName: String,
    val caps: List<String>,
    /** Admin edition: the relay sends this phone the device registry (ignored when device security decides roles). */
    val isAdmin: Boolean = false,
    val platform: String = "android",
    val type: String = MessageType.REGISTER,
)

/** Asks for the device registry (answered with `host-list` to administrators only). */
data class GetHosts(val type: String = MessageType.GET_HOSTS)

/** Administrators: names a device in the relay's registry for every administrator (empty clears the name). */
data class HostRename(val targetClean: String, val alias: String, val type: String = MessageType.HOST_RENAME)

/** Admin edition: replaces the hosts on the monitor wall (clean IDs, at most 16; empty stops watching). */
data class MonitorSet(val targets: List<String>, val type: String = MessageType.MONITOR_SET)

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
 * shortcut, switch-monitor, set-mic, set-resolution.
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
    /** `set-resolution`: preset (1080, what hosts without automatic mode keep), `auto` and the phone's box. */
    val mode: Int? = null,
    val auto: Boolean? = null,
    val width: Int? = null,
    val height: Int? = null,
    /** `set-performance`: "low" / "medium" / "high", and the frame-rate cap. */
    val preset: String? = null,
    val maxFps: Int? = null,
    /** `keycombo`: modifiers held around [keyCode]. */
    val ctrl: Boolean? = null,
    val alt: Boolean? = null,
    val shift: Boolean? = null,
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
    /** `monitor-state`: the host one monitor-wall tile shows. */
    val targetClean: String? = null,
    /** `monitor-state`: `live`, `offline`, `unsupported` or `denied` (see `domain/MonitorState`). */
    val state: String? = null,
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

/** Received control event (`input-command.event`). A viewer acts on `set-mic` and `text-focus`. */
data class RemoteEvent(
    val action: String? = null,
    val muted: Boolean? = null,
    /** `text-focus`: focus is in a host text field, at this rect of the streamed monitor (0..1). */
    val editable: Boolean? = null,
    val normX: Float? = null,
    val normY: Float? = null,
    val normW: Float? = null,
    val normH: Float? = null,
    val password: Boolean? = null,
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
