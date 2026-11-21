package com.opendesk.app.domain

/**
 * Capabilities this app lists in `register.caps` (see docs/protocol.md#capabilities at the repository root).
 *
 * Android receives framed JPEG key and delta video (`video-delta`), H.264 during motion when the phone has a
 * hardware decoder (`video-h264`), and the single-session message shapes. Framed video is what end-to-end
 * encryption seals; legacy plain JPEGs stay readable by the relay.
 */
object Capabilities {
    /** This device only views other machines and can never be controlled; relays and desktops must not offer it as a host. */
    const val VIEWER_ONLY = "viewer-only"

    /** Framed key/delta video (see `domain/FramedVideo`). */
    const val VIDEO_DELTA = "video-delta"

    /** H.264 access units inside the framed format (see `ui/viewer/H264Decoder`). */
    const val VIDEO_H264 = "video-h264"

    /** End-to-end encryption with the host (see `session/E2eChannel`). */
    const val E2E = "e2e"

    /** Relay capability: forwards `p2p-signal`, which carries the end-to-end handshake. */
    const val RELAY_P2P_SIGNAL = "p2p-signal"

    /** Admin edition: shows the monitor wall (`monitor-state` and `0x11` thumbnails). */
    const val MONITOR_VIEW = "monitor-view"

    /** Relay capability: runs the monitor wall. */
    const val RELAY_MONITOR = "monitor"

    /**
     * The host tells this viewer when keyboard focus enters or leaves a text field (`text-focus` events), so the
     * mini keyboard comes up by itself (see `ui/viewer/keyboard/AutoKeyboard`).
     */
    const val TEXT_FOCUS = "text-focus"

    /** At most this many frames per second (`fps-N`): what a phone shows smoothly without draining the battery. */
    const val MAX_FPS = "fps-60"

    /**
     * Motion goes to H.264 at once, at the normal bitrate (`h264-eager`): the phone's hardware decoder makes H.264
     * far cheaper than JPEG. Listed only with `video-h264`. Hosts that list it too honour it; older hosts are asked
     * for the High preset instead (see [performanceFor]).
     */
    const val H264_EAGER = "h264-eager"

    /**
     * Everything `register.caps` carries; `h264` when this phone decodes H.264 in hardware and it has not failed.
     * `fps-N` stays last (relays up to 1.2.0 keep only the first 16 names).
     */
    fun advertised(h264: Boolean): List<String> =
        listOf(VIEWER_ONLY, VIDEO_DELTA, E2E, TEXT_FOCUS) + (if (h264) listOf(VIDEO_H264, H264_EAGER) else emptyList()) +
            (if (Edition.isAdmin) listOf(MONITOR_VIEW) else emptyList()) +
            listOf(MAX_FPS)

    /**
     * The `set-performance` preset to send once connected, or null to leave the host's default (Medium): Low with
     * data saver; High toward a host that predates `h264-eager` while this phone decodes H.264 (High is what makes
     * such a host start H.264 at once, at twice the bitrate); otherwise none.
     */
    fun performanceFor(h264: Boolean, hostCaps: List<String>, dataSaver: Boolean): String? = when {
        dataSaver -> "low"
        h264 && H264_EAGER !in hostCaps -> "high"
        else -> null
    }
}
