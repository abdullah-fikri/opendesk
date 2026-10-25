package com.opendesk.app.domain

import kotlin.math.abs

/** Desk IDs: nine digits without a leading zero, displayed as "123 456 789". */
object DeskId {
    private const val LENGTH = 9

    /** Keeps only the digits ("123 456 789" -> "123456789"). */
    fun clean(raw: String): String = raw.filter { it.isDigit() }

    /** Display form "123 456 789"; shorter input comes back as bare digits. */
    fun format(raw: String): String {
        val digits = clean(raw)
        if (digits.length < LENGTH) return digits
        return "${digits.substring(0, 3)} ${digits.substring(3, 6)} ${digits.substring(6, 9)}"
    }

    /**
     * Derives a stable desk ID from a device identifier (djb2 hash mapped into 100 000 000..999 999 999).
     * The same input always yields the same ID, so reinstalling keeps the device's ID.
     */
    fun fromDeviceIdentifier(identifier: String): String {
        var hash = 5381L
        for (char in identifier) {
            hash = ((hash shl 5) + hash) + char.code.toLong()
            hash = hash and 0x7FFFFFFF
        }
        return format(((abs(hash) % 900_000_000) + 100_000_000).toString())
    }
}
