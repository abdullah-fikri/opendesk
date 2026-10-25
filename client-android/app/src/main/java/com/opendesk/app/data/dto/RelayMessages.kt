package com.opendesk.app.data.dto

import com.google.gson.JsonObject

/*
 * Wire format between the Android client and the relay (see docs/protocol.md at the repository root).
 * Outbound messages are plain data classes serialized by Gson; null fields are omitted.
 * Android is a viewer only. It advertises `viewer-only` and no video or multi-session capability, so it always
 * receives plain JPEG frames and the single-session message shapes.
 */

/** Message `type` values. */
object MessageType {
    const val REGISTER = "register"
    const val CONNECT_REQUEST = "connect-request"
    const val APPROVAL_RESPONSE = "approval-response"
    const val SESSION_DISCONNECT = "session-disconnect"
    const val RELAY_INPUT = "relay-input"

    const val REGISTERED_ACK = "registered-ack"
    const val INCOMING_REQUEST = "incoming-request"
    const val SESSION_CONNECTED = "session-connected"
    const val INPUT_COMMAND = "input-command"
    const val FILE_TRANSFER = "file-transfer"
    const val SESSION_CLOSED = "session-closed"
    const val ERROR = "error"
}

/** First message on every connection. */
data class RegisterRequest(
    val deskId: String,
    val deviceToken: String,
    val deviceName: String,
    val caps: List<String>,
    val platform: String = "android",
    val type: String = MessageType.REGISTER,
)

/** Opens a viewer session to `targetId`. */
data class ConnectRequest(val targetId: String, val type: String = MessageType.CONNECT_REQUEST)

/** Answer to an `incoming-request`. Android never hosts, so it only ever sends `accept = false`. */
data class ApprovalResponse(val fromClean: String, val accept: Boolean, val type: String = MessageType.APPROVAL_RESPONSE)

/** Ends the session with `targetClean`. */
data class SessionDisconnect(val targetClean: String, val type: String = MessageType.SESSION_DISCONNECT)

/** An input event for the viewed host. */
data class RelayInput(val targetClean: String, val event: InputEvent, val type: String = MessageType.RELAY_INPUT)

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
