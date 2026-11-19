package com.opendesk.app.domain

/**
 * Capabilities this app lists in `register.caps` (see docs/protocol.md#capabilities at the repository root).
 *
 * Android receives framed JPEG key and delta video (`video-delta`, no H.264) and the single-session message shapes.
 * Framed video is what end-to-end encryption seals; legacy plain JPEGs stay readable by the relay.
 */
object Capabilities {
    /** This device only views other machines and can never be controlled; relays and desktops must not offer it as a host. */
    const val VIEWER_ONLY = "viewer-only"

    /** Framed key/delta video (see `domain/FramedVideo`). */
    const val VIDEO_DELTA = "video-delta"

    /** End-to-end encryption with the host (see `session/E2eChannel`). */
    const val E2E = "e2e"

    /** Relay capability: forwards `p2p-signal`, which carries the end-to-end handshake. */
    const val RELAY_P2P_SIGNAL = "p2p-signal"

    /** Admin edition: shows the monitor wall (`monitor-state` and `0x11` thumbnails). */
    const val MONITOR_VIEW = "monitor-view"

    /** Relay capability: runs the monitor wall. */
    const val RELAY_MONITOR = "monitor"

    /** Everything `register.caps` carries. */
    val advertised: List<String> = listOf(VIEWER_ONLY, VIDEO_DELTA, E2E) + if (Edition.isAdmin) listOf(MONITOR_VIEW) else emptyList()
}
