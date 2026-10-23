package com.opendesk.app.domain

/** Relay address rules, identical to the desktop client's. */
object GatewayUrl {
    private val privatePrefixes = listOf("127.", "192.168.", "10.", "localhost")

    /**
     * Turns user input into a WebSocket URL: http(s) becomes ws(s), private addresses default to ws://,
     * everything else to wss://. Returns an empty string for empty input.
     */
    fun normalize(raw: String): String {
        val trimmed = raw.trim().trimEnd('/')
        return when {
            trimmed.isEmpty() -> ""
            trimmed.startsWith("https://") -> "wss://" + trimmed.removePrefix("https://")
            trimmed.startsWith("http://") -> "ws://" + trimmed.removePrefix("http://")
            trimmed.startsWith("ws://") || trimmed.startsWith("wss://") -> trimmed
            privatePrefixes.any { trimmed.startsWith(it) } -> "ws://$trimmed"
            else -> "wss://$trimmed"
        }
    }
}
