package com.opendesk.app.data.network

import android.os.Handler
import android.os.Looper
import com.google.gson.Gson
import com.opendesk.app.data.dto.MessageType
import com.opendesk.app.data.dto.RegisterRequest
import com.opendesk.app.data.dto.ServerMessage
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import java.util.concurrent.TimeUnit

/**
 * WebSocket transport to the relay. Registers on open, decodes text messages into [ServerMessage] and hands
 * binary messages (voice and video) over untouched. Text messages and status changes are delivered on the main
 * thread; binary messages stay on the socket thread so video never waits behind the UI.
 */
class GatewayClient(
    private val registration: RegisterRequest,
    private val listener: Listener,
) {
    /** Receives everything the relay sends. */
    interface Listener {
        /** Main thread. */
        fun onConnectionChanged(online: Boolean)

        /** Main thread. */
        fun onMessage(message: ServerMessage)

        /** Socket thread. */
        fun onBinary(bytes: ByteArray)
    }

    private val http = OkHttpClient.Builder()
        .readTimeout(0, TimeUnit.MILLISECONDS)
        // The relay pings every 15 s; OkHttp pings too so a dead network is noticed from this side.
        .pingInterval(PING_INTERVAL_SECONDS, TimeUnit.SECONDS)
        .build()
    private val gson = Gson()
    private val mainHandler = Handler(Looper.getMainLooper())
    private var webSocket: WebSocket? = null

    /** Set by [disconnect]: a client closed on purpose reports nothing more, so it cannot overwrite the status of its replacement. */
    @Volatile
    private var closedByUs = false

    /** Connects to `url` (already normalized to ws:// or wss://). */
    fun connect(url: String) {
        val request = try {
            Request.Builder().url(url).build()
        } catch (e: IllegalArgumentException) {
            mainHandler.post { listener.onConnectionChanged(false) }
            return
        }
        webSocket = http.newWebSocket(request, object : WebSocketListener() {
            override fun onOpen(ws: WebSocket, response: Response) {
                ws.send(gson.toJson(registration))
            }

            override fun onMessage(ws: WebSocket, bytes: ByteString) {
                if (!closedByUs) listener.onBinary(bytes.toByteArray())
            }

            override fun onMessage(ws: WebSocket, text: String) {
                if (closedByUs) return
                val message = runCatching { gson.fromJson(text, ServerMessage::class.java) }.getOrNull() ?: return
                if (message.type == MessageType.REGISTERED_ACK) {
                    mainHandler.post { listener.onConnectionChanged(true) }
                }
                mainHandler.post { listener.onMessage(message) }
            }

            override fun onClosed(ws: WebSocket, code: Int, reason: String) {
                if (!closedByUs) mainHandler.post { listener.onConnectionChanged(false) }
            }

            override fun onFailure(ws: WebSocket, t: Throwable, response: Response?) {
                if (!closedByUs) mainHandler.post { listener.onConnectionChanged(false) }
            }
        })
    }

    /** Sends a DTO as JSON text. */
    fun send(message: Any) {
        webSocket?.send(gson.toJson(message))
    }

    /** Sends a binary message (voice or video). */
    fun sendBinary(bytes: ByteArray) {
        webSocket?.send(bytes.toByteString(0, bytes.size))
    }

    /** Closes the connection normally. The listener hears nothing more from this client. */
    fun disconnect() {
        closedByUs = true
        webSocket?.close(NORMAL_CLOSURE, null)
        webSocket = null
    }

    private companion object {
        const val PING_INTERVAL_SECONDS = 15L
        const val NORMAL_CLOSURE = 1000
    }
}
