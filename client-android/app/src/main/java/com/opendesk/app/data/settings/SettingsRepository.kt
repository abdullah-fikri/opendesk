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

    /**
     * Device credential (see docs/features/device-security.md at the repository root): issued by the relay when it
     * enrolls this device, or set by hand on older installs; may be empty.
     */
    var deviceToken: String
        get() = prefs.getString(KEY_DEVICE_TOKEN, "") ?: ""
        set(value) = prefs.edit().putString(KEY_DEVICE_TOKEN, value).apply()

    /** Relay that issued [deviceToken]; empty for a token set by hand, which every relay receives. */
    var deviceTokenRelay: String
        get() = prefs.getString(KEY_DEVICE_TOKEN_RELAY, "") ?: ""
        set(value) = prefs.edit().putString(KEY_DEVICE_TOKEN_RELAY, value).apply()

    /** The token to present to the configured relay: an issued one goes back to its own relay only. */
    val tokenForGateway: String
        get() = deviceTokenRelay.let { relay -> if (relay.isEmpty() || relay == gatewayUrl) deviceToken else "" }

    /** Stores the token the configured relay just issued. */
    fun storeIssuedToken(token: String) {
        prefs.edit().putString(KEY_DEVICE_TOKEN, token).putString(KEY_DEVICE_TOKEN_RELAY, gatewayUrl).apply()
    }

    /**
     * Administrator mode: registers with `isAdmin` so the relay sends the device registry, and Devices lists every
     * computer on the relay with its presence. Off: Devices lists the computers this phone connected to.
     */
    var adminMode: Boolean
        get() = prefs.getBoolean(KEY_ADMIN_MODE, false)
        set(value) = prefs.edit().putBoolean(KEY_ADMIN_MODE, value).apply()

    /** Light or dark: [APPEARANCE_SYSTEM] (follow the phone, the default), [APPEARANCE_LIGHT] or [APPEARANCE_DARK]. */
    var appearance: Int
        get() = prefs.getInt(KEY_APPEARANCE, APPEARANCE_SYSTEM)
        set(value) = prefs.edit().putInt(KEY_APPEARANCE, value).apply()

    /** The touch-gesture guide was shown once in a remote session. */
    var gesturesSeen: Boolean
        get() = prefs.getBoolean(KEY_GESTURES_SEEN, false)
        set(value) = prefs.edit().putBoolean(KEY_GESTURES_SEEN, value).apply()

    /** This device's desk ID ("123 456 789"), derived from ANDROID_ID once and then kept. */
    val deskId: String
        get() = prefs.getString(KEY_DESK_ID, null) ?: DeskId.fromDeviceIdentifier(androidId ?: FALLBACK_IDENTIFIER).also {
            prefs.edit().putString(KEY_DESK_ID, it).apply()
        }

    companion object {
        const val APPEARANCE_SYSTEM = 0
        const val APPEARANCE_LIGHT = 1
        const val APPEARANCE_DARK = 2

        private const val PREFS_NAME = "opendesk_prefs"
        private const val KEY_GATEWAY_URL = "gateway_url"
        private const val KEY_DEVICE_TOKEN = "device_token"
        private const val KEY_DEVICE_TOKEN_RELAY = "device_token_relay"
        private const val KEY_ADMIN_MODE = "admin_mode"
        private const val KEY_GESTURES_SEEN = "gestures_seen"
        private const val KEY_APPEARANCE = "appearance"
        // Key name kept from earlier builds so existing installs keep their ID.
        private const val KEY_DESK_ID = "hardware_desk_id"
        private const val FALLBACK_IDENTIFIER = "android_opendesk"
    }
}
