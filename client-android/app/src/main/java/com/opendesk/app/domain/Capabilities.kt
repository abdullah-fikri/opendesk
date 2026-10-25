package com.opendesk.app.domain

/**
 * Capabilities this app lists in `register.caps` (see docs/protocol.md#capabilities at the repository root).
 *
 * Android advertises no video capability on purpose: it always receives plain JPEG frames and the single-session
 * message shapes.
 */
object Capabilities {
    /** This device only views other machines and can never be controlled; relays and desktops must not offer it as a host. */
    const val VIEWER_ONLY = "viewer-only"

    /** Everything `register.caps` carries. */
    val advertised: List<String> = listOf(VIEWER_ONLY)
}
