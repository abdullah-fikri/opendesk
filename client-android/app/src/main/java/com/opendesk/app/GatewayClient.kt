package com.opendesk.app

import android.os.Handler
import android.os.Looper
import com.google.gson.Gson
import com.google.gson.JsonObject
import okhttp3.*
import okio.ByteString
import okio.ByteString.Companion.toByteString
import java.util.concurrent.TimeUnit

class GatewayClient(
    private val deskId: String,
    private val onMessageListener: (JsonObject) -> Unit,
    private val onStatusListener: (Boolean) -> Unit
) {
    private val client = OkHttpClient.Builder()
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .pingInterval(15, TimeUnit.SECONDS)
        .build()

    private var webSocket: WebSocket? = null
    private val gson = Gson()
    private val mainHandler = Handler(Looper.getMainLooper())
    private var isConnected = false

    fun connect(rawGatewayUrl: String) {
        val trimmed = rawGatewayUrl.trim().trimEnd('/')
        val gatewayUrl = when {
            trimmed.startsWith("ws://") || trimmed.startsWith("wss://") ||
            trimmed.startsWith("http://") || trimmed.startsWith("https://") -> trimmed
            trimmed.startsWith("127.") || trimmed.startsWith("192.168.") ||
            trimmed.startsWith("10.") || trimmed.startsWith("localhost") -> "ws://$trimmed"
            else -> "wss://$trimmed"
        }

        val request = try {
            Request.Builder()
                .url(gatewayUrl)
                .build()
        } catch (e: Exception) {
            mainHandler.post { onStatusListener(false) }
            return
        }

        webSocket = client.newWebSocket(request, object : WebSocketListener() {
            override fun onOpen(ws: WebSocket, response: Response) {
                isConnected = true
                mainHandler.post { onStatusListener(true) }

                val reg = JsonObject().apply {
                    addProperty("type", "register")
                    addProperty("deskId", deskId)
                    addProperty("platform", "android")
                    addProperty("deviceName", "Android HP ($deskId)")
                }
                ws.send(gson.toJson(reg))
            }

            override fun onMessage(ws: WebSocket, bytes: ByteString) {
                if (bytes.size > 1 && bytes[0] == 0x02.toByte()) {
                    // Real-time 16kHz Voice Call PCM packet
                    VoiceCallManager.playPcmChunk(bytes.substring(1).toByteArray())
                } else {
                    // Zero-copy binary video frame fast-path (never touches UI thread queue)
                    RemoteViewerActivity.instance?.enqueueBinaryFrame(bytes.toByteArray())
                }
            }

            override fun onMessage(ws: WebSocket, text: String) {
                try {
                    val json = gson.fromJson(text, JsonObject::class.java) ?: return
                    val type = json.get("type")?.asString ?: return
                    if (type == "frame") {
                        // Legacy Base64 frame fast-path (bypass mainHandler UI queue!)
                        val data = json.get("data")?.asString ?: return
                        RemoteViewerActivity.instance?.enqueueBase64Frame(data)
                        return
                    }
                    mainHandler.post { onMessageListener(json) }
                } catch (e: Exception) {}
            }

            override fun onClosed(ws: WebSocket, code: Int, reason: String) {
                isConnected = false
                mainHandler.post { onStatusListener(false) }
            }

            override fun onFailure(ws: WebSocket, t: Throwable, response: Response?) {
                isConnected = false
                mainHandler.post { onStatusListener(false) }
            }
        })
    }

    fun send(json: JsonObject) {
        webSocket?.send(gson.toJson(json))
    }

    fun sendBinary(bytes: ByteArray) {
        webSocket?.send(bytes.toByteString(0, bytes.size))
    }

    fun disconnect() {
        webSocket?.close(1000, "Normal closure")
        webSocket = null
        isConnected = false
    }
}
