package com.opendesk.app.domain

import java.security.MessageDigest

/**
 * Short form of a device key for people to compare, the same as the desktop's `opendesk status`: the first 10 bytes
 * of its SHA-256 as 5 groups of 4 hex digits (`3f9a 01c2 ...`).
 */
object KeyFingerprint {
    fun of(publicKey: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(publicKey).take(10)
            .joinToString("") { "%02x".format(it) }
            .chunked(4)
            .joinToString(" ")
}
