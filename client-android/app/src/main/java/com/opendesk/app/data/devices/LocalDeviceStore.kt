package com.opendesk.app.data.devices

import android.content.Context
import com.opendesk.app.domain.DeskId

/** What this phone remembers about computers on its own: names given here and computers deleted from the list. */
class LocalDeviceStore(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** The name given to `cleanId` on this phone, if any. */
    fun alias(cleanId: String): String? = prefs.getString(ALIAS_PREFIX + DeskId.clean(cleanId), null)?.takeIf { it.isNotEmpty() }

    /** Names `cleanId` on this phone; an empty name clears it. */
    fun setAlias(cleanId: String, name: String) {
        val key = ALIAS_PREFIX + DeskId.clean(cleanId)
        val trimmed = name.trim()
        prefs.edit().apply { if (trimmed.isEmpty()) remove(key) else putString(key, trimmed) }.apply()
    }

    fun hidden(): Set<String> = prefs.getStringSet(KEY_HIDDEN, emptySet()).orEmpty().toSet()

    /** Hides `cleanId` and forgets its name. */
    fun hide(cleanId: String) {
        val id = DeskId.clean(cleanId)
        prefs.edit().remove(ALIAS_PREFIX + id).putStringSet(KEY_HIDDEN, hidden() + id).apply()
    }

    fun unhide(cleanId: String) {
        val id = DeskId.clean(cleanId)
        val hidden = hidden()
        if (id in hidden) prefs.edit().putStringSet(KEY_HIDDEN, hidden - id).apply()
    }

    private companion object {
        const val PREFS_NAME = "opendesk_devices"
        const val ALIAS_PREFIX = "alias_"
        const val KEY_HIDDEN = "hidden"
    }
}
