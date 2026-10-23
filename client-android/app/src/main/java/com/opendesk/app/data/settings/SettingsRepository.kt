package com.opendesk.app.data.settings

import android.content.Context
import android.provider.Settings
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.GatewayUrl

/** Persistent client settings in `SharedPreferences`. */
class SettingsRepository(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
    private val androidId: String? = Settings.Secure.getString(context.contentResolver, Settings.Secure.ANDROID_ID)

    /** Relay URL, normalized; empty until the user configures one. */
    var gatewayUrl: String
        get() = prefs.getString(KEY_GATEWAY_URL, "") ?: ""
        set(value) = prefs.edit().putString(KEY_GATEWAY_URL, GatewayUrl.normalize(value)).apply()

    /** Device credential registered on the relay (see server-relay/docs/provisioning.md); may be empty. */
    var deviceToken: String
        get() = prefs.getString(KEY_DEVICE_TOKEN, "") ?: ""
        set(value) = prefs.edit().putString(KEY_DEVICE_TOKEN, value).apply()

    /** Whether the first-run accessibility prompt was shown. */
    var promptedForAccessibility: Boolean
        get() = prefs.getBoolean(KEY_PROMPTED_ACCESSIBILITY, false)
        set(value) = prefs.edit().putBoolean(KEY_PROMPTED_ACCESSIBILITY, value).apply()

    /** This device's desk ID ("123 456 789"), derived from ANDROID_ID once and then kept. */
    val deskId: String
        get() = prefs.getString(KEY_DESK_ID, null) ?: DeskId.fromDeviceIdentifier(androidId ?: FALLBACK_IDENTIFIER).also {
            prefs.edit().putString(KEY_DESK_ID, it).apply()
        }

    private companion object {
        const val PREFS_NAME = "opendesk_prefs"
        const val KEY_GATEWAY_URL = "gateway_url"
        const val KEY_DEVICE_TOKEN = "device_token"
        const val KEY_PROMPTED_ACCESSIBILITY = "has_prompted_accessibility"
        // Key name kept from earlier builds so existing installs keep their ID.
        const val KEY_DESK_ID = "hardware_desk_id"
        const val FALLBACK_IDENTIFIER = "android_opendesk"
    }
}
