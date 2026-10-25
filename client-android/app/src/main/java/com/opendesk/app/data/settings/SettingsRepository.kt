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

    /** This device's desk ID ("123 456 789"), derived from ANDROID_ID once and then kept. */
    val deskId: String
        get() = prefs.getString(KEY_DESK_ID, null) ?: DeskId.fromDeviceIdentifier(androidId ?: FALLBACK_IDENTIFIER).also {
            prefs.edit().putString(KEY_DESK_ID, it).apply()
        }

    private companion object {
        const val PREFS_NAME = "opendesk_prefs"
        const val KEY_GATEWAY_URL = "gateway_url"
        const val KEY_DEVICE_TOKEN = "device_token"
        const val KEY_DEVICE_TOKEN_RELAY = "device_token_relay"
        // Key name kept from earlier builds so existing installs keep their ID.
        const val KEY_DESK_ID = "hardware_desk_id"
        const val FALLBACK_IDENTIFIER = "android_opendesk"
    }
}
