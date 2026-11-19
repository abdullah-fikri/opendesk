package com.opendesk.app.domain

/** A computer as the Devices list shows it. `online` is null when this phone cannot know (outside administrator mode). */
data class DeviceItem(val cleanId: String, val name: String, val platform: String, val online: Boolean?) {
    val formattedId: String get() = DeskId.format(cleanId)

    /** Search: matches a name (any case) or the digits of an ID. */
    fun matches(query: String): Boolean {
        val q = query.trim()
        if (q.isEmpty()) return true
        val digits = DeskId.clean(q)
        return name.contains(q, ignoreCase = true) ||
            (digits.isNotEmpty() && digits.length == q.count { !it.isWhitespace() } && cleanId.contains(digits))
    }
}
