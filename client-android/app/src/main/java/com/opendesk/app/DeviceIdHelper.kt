package com.opendesk.app

import android.content.Context
import android.provider.Settings
import kotlin.math.abs

object DeviceIdHelper {
    fun getDeskId(context: Context): String {
        val prefs = context.getSharedPreferences("opendesk_prefs", Context.MODE_PRIVATE)
        val cached = prefs.getString("hardware_desk_id", null)
        if (cached != null) return cached

        val androidId = Settings.Secure.getString(context.contentResolver, Settings.Secure.ANDROID_ID) ?: "android_opendesk"
        var hash = 5381L
        for (char in androidId) {
            hash = ((hash shl 5) + hash) + char.code.toLong()
            hash = hash and 0x7FFFFFFF
        }
        val num = (abs(hash) % 900000000) + 100000000
        val raw = num.toString()
        val formatted = "${raw.substring(0, 3)} ${raw.substring(3, 6)} ${raw.substring(6, 9)}"

        prefs.edit().putString("hardware_desk_id", formatted).apply()
        return formatted
    }
}
