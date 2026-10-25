package com.opendesk.app.domain

/**
 * Relay address rules. Like the desktop client, `http(s)://` becomes `ws(s)://`; unlike it, only this device's
 * loopback defaults to plaintext `ws://`, because release builds refuse cleartext to any other host (see
 * `res/xml/network_security_config.xml`) and the relay refuses plaintext from remote addresses too.
 */
object GatewayUrl {
    private val loopbackHosts = listOf("localhost", "127.", "10.0.2.2")

    /**
     * Turns user input into a WebSocket URL: http(s) becomes ws(s), loopback addresses default to ws://,
     * everything else to wss://. Returns an empty string for empty input.
     */
    fun normalize(raw: String): String {
        val trimmed = raw.trim().trimEnd('/')
        return when {
            trimmed.isEmpty() -> ""
            trimmed.startsWith("https://") -> "wss://" + trimmed.removePrefix("https://")
            trimmed.startsWith("http://") -> "ws://" + trimmed.removePrefix("http://")
            trimmed.startsWith("ws://") || trimmed.startsWith("wss://") -> trimmed
            loopbackHosts.any { trimmed.startsWith(it) } -> "ws://$trimmed"
            else -> "wss://$trimmed"
        }
    }

    /** Whether `url` (already normalized) is plaintext. */
    fun isPlaintext(url: String): Boolean = url.startsWith("ws://")

    /** Host part of a normalized URL ("wss://relay.example.com:443/x" -> "relay.example.com"). */
    fun host(url: String): String {
        val rest = url.substringAfter("://")
        val authority = rest.substringBefore('/').substringAfter('@')
        return if (authority.startsWith("[")) authority.substringBefore(']') + "]" else authority.substringBefore(':')
    }
}
