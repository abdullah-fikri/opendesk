package com.opendesk.app.ui.main

import android.view.View
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.DeviceItem
import com.opendesk.app.ui.devices.DeviceDirectory

/** Where a connection attempt stands. */
sealed interface ConnectState {
    object Ready : ConnectState
    object Connecting : ConnectState
    data class Failed(val message: String) : ConnectState
}

/** What the main screen's pages ask of the activity. */
interface MainHost {
    val settings: SettingsRepository
    val history: SessionHistory
    val directory: DeviceDirectory
    val isTablet: Boolean

    /** The computer the current (or last) connection attempt is for, and where it stands. */
    val target: DeviceItem?
    val connectState: ConnectState

    /** When the relay last came online (`SystemClock.uptimeMillis`). */
    val onlineSinceMs: Long

    /** Phone: opens the connect sheet; tablet: shows the computer in the detail pane. */
    fun open(item: DeviceItem)

    /** Long press: the Connect / Rename / Delete menu over `anchor`, lifting `preview` (a copy of the row). */
    fun showMenu(anchor: View, item: DeviceItem, preview: View)

    /** Starts connecting to `item`. */
    fun connect(item: DeviceItem)

    /** Abandons a pending attempt (the relay may still answer; the answer is then ignored). */
    fun cancelConnect()

    fun showSessions()
    fun showRelay()
    fun showSecurity()
    fun showSettings()

    /** Reconnects with the stored relay settings. */
    fun restartRelay()
}
