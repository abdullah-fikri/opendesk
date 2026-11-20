package com.opendesk.app.data.settings

import android.content.Context
import android.provider.Settings
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.Edition
import com.opendesk.app.domain.GatewayUrl
import com.opendesk.app.domain.MonitorSettings
import com.opendesk.app.domain.MonitorWall

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


    /** Light or dark: [APPEARANCE_SYSTEM] (follow the phone, the default), [APPEARANCE_LIGHT] or [APPEARANCE_DARK]. */
    var appearance: Int
        get() = prefs.getInt(KEY_APPEARANCE, APPEARANCE_SYSTEM)
        set(value) = prefs.edit().putInt(KEY_APPEARANCE, value).apply()

    /** Admin edition: the monitor wall's settings; a grid never chosen is `defaultGrid` (by screen size). */
    fun monitor(defaultGrid: Int): MonitorSettings = MonitorSettings(
        grid = prefs.getInt(KEY_MONITOR_GRID, 0).takeIf { it > 0 } ?: defaultGrid,
        rotate = prefs.getBoolean(KEY_MONITOR_ROTATE, false),
        rotateSecs = prefs.getInt(KEY_MONITOR_ROTATE_SECS, MonitorWall.ROTATE_DEFAULT_SECS),
        all = prefs.getBoolean(KEY_MONITOR_ALL, true),
        selected = prefs.getStringSet(KEY_MONITOR_SELECTED, null)?.toSet().orEmpty(),
    )

    fun saveMonitor(value: MonitorSettings) {
        prefs.edit()
            .putInt(KEY_MONITOR_GRID, value.grid)
            .putBoolean(KEY_MONITOR_ROTATE, value.rotate)
            .putInt(KEY_MONITOR_ROTATE_SECS, value.rotateSecs)
            .putBoolean(KEY_MONITOR_ALL, value.all)
            .putStringSet(KEY_MONITOR_SELECTED, value.selected.toSet())
            .apply()
    }

    /** Whether the remote screen fills the phone's screen (no bands) in this orientation; landscape fills by default. */
    fun viewerFill(landscape: Boolean): Boolean =
        prefs.getBoolean(if (landscape) KEY_FILL_LANDSCAPE else KEY_FILL_PORTRAIT, landscape)

    fun setViewerFill(landscape: Boolean, fill: Boolean) =
        prefs.edit().putBoolean(if (landscape) KEY_FILL_LANDSCAPE else KEY_FILL_PORTRAIT, fill).apply()

    /** The touch-gesture guide was shown once in a remote session. */
    var gesturesSeen: Boolean
        get() = prefs.getBoolean(KEY_GESTURES_SEEN, false)
        set(value) = prefs.edit().putBoolean(KEY_GESTURES_SEEN, value).apply()

    /**
     * This device's desk ID ("123 456 789"), derived from ANDROID_ID once and then kept. ANDROID_ID is the same for
     * every app signed with the same key, so the admin edition derives its own: both editions on one phone must not
     * register the same ID.
     */
    val deskId: String
        get() = prefs.getString(KEY_DESK_ID, null) ?: DeskId.fromDeviceIdentifier(deviceIdentifier()).also {
            prefs.edit().putString(KEY_DESK_ID, it).apply()
        }

    private fun deviceIdentifier(): String =
        (androidId ?: FALLBACK_IDENTIFIER).let { if (Edition.isAdmin) "$it:admin" else it }

    init {
        // Administrator mode used to be a switch; the edition decides it now.
        if (prefs.contains(KEY_LEGACY_ADMIN_MODE)) prefs.edit().remove(KEY_LEGACY_ADMIN_MODE).apply()
    }

    companion object {
        const val APPEARANCE_SYSTEM = 0
        const val APPEARANCE_LIGHT = 1
        const val APPEARANCE_DARK = 2

        private const val PREFS_NAME = "opendesk_prefs"
        private const val KEY_GATEWAY_URL = "gateway_url"
        private const val KEY_DEVICE_TOKEN = "device_token"
        private const val KEY_DEVICE_TOKEN_RELAY = "device_token_relay"
        private const val KEY_LEGACY_ADMIN_MODE = "admin_mode"
        private const val KEY_GESTURES_SEEN = "gestures_seen"
        private const val KEY_FILL_LANDSCAPE = "viewer_fill_landscape"
        private const val KEY_FILL_PORTRAIT = "viewer_fill_portrait"
        private const val KEY_APPEARANCE = "appearance"
        private const val KEY_MONITOR_GRID = "monitor_grid"
        private const val KEY_MONITOR_ROTATE = "monitor_rotate"
        private const val KEY_MONITOR_ROTATE_SECS = "monitor_rotate_secs"
        private const val KEY_MONITOR_ALL = "monitor_all"
        private const val KEY_MONITOR_SELECTED = "monitor_selected"
        // Key name kept from earlier builds so existing installs keep their ID.
        private const val KEY_DESK_ID = "hardware_desk_id"
        private const val FALLBACK_IDENTIFIER = "android_opendesk"
    }
}
