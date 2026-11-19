package com.opendesk.app.ui.main

import android.content.Intent
import android.content.res.Configuration
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.widget.ImageView
import android.widget.TextView
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.opendesk.app.R
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.DeviceItem
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Alerts
import com.opendesk.app.ui.common.Ui
import com.opendesk.app.ui.devices.ConnectSheet
import com.opendesk.app.ui.devices.DeviceDirectory
import com.opendesk.app.ui.devices.DeviceMenu
import com.opendesk.app.ui.devices.DevicesPage
import com.opendesk.app.ui.sessions.SessionsPage
import com.opendesk.app.ui.settings.RelayPage
import com.opendesk.app.ui.settings.SecurityPage
import com.opendesk.app.ui.settings.SettingsPage
import com.opendesk.app.ui.viewer.RemoteViewerActivity

/**
 * Home screen: Devices, Sessions and Settings (with its Relay and Security pages). Phones get a bottom bar and a
 * connect sheet; tablets (sw600dp) a navigation rail and a list beside the selected computer. Light or dark follows
 * the phone's setting (DayNight resources), and the screen is recreated when that setting changes.
 */
class MainActivity : AppCompatActivity(), RelayConnection.Listener, MainHost {

    override lateinit var settings: SettingsRepository
    override lateinit var history: SessionHistory
    override lateinit var directory: DeviceDirectory
    override val isTablet: Boolean get() = resources.getBoolean(R.bool.is_tablet)

    override var target: DeviceItem? = null
        private set
    override var connectState: ConnectState = ConnectState.Ready
        private set
    override var onlineSinceMs: Long = 0L
        private set

    private lateinit var content: ViewGroup
    private lateinit var sheet: ConnectSheet
    private var page = PAGE_DEVICES

    private var devices: DevicesPage? = null
    private var sessions: SessionsPage? = null
    private var settingsPage: SettingsPage? = null
    private var relayPage: RelayPage? = null
    private var securityPage: SecurityPage? = null

    private val handler = Handler(Looper.getMainLooper())
    private val connectTimeout = Runnable { failConnect(getString(R.string.connect_failed_body)) }
    private val registryWait = Runnable { devices?.refresh() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        setContentView(R.layout.activity_main)
        settings = SettingsRepository(this)
        history = SessionHistory(this)
        directory = DeviceDirectory(this, settings, history)
        sheet = ConnectSheet(this, this)
        content = findViewById(R.id.content)

        val night = (resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES
        WindowInsetsControllerCompat(window, window.decorView).apply {
            isAppearanceLightStatusBars = !night
            isAppearanceLightNavigationBars = !night
        }
        applyInsets()
        setupNav()

        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                when (page) {
                    PAGE_RELAY, PAGE_SECURITY -> showPage(PAGE_SETTINGS)
                    PAGE_SESSIONS, PAGE_SETTINGS -> showPage(PAGE_DEVICES)
                    else -> {
                        isEnabled = false
                        onBackPressedDispatcher.onBackPressed()
                    }
                }
            }
        })

        RelayConnection.listener = this
        // The connection is process-wide: a recreated activity (rotation, theme change) keeps the running one.
        if (!RelayConnection.isStarted) connectRelay()
        // First start without a relay: nothing to connect to until one is set.
        val first = if (settings.gatewayUrl.isEmpty() || !SettingsPage.permitted(settings.gatewayUrl)) PAGE_RELAY else PAGE_DEVICES
        showPage(savedInstanceState?.getInt(STATE_PAGE, first) ?: first)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putInt(STATE_PAGE, page)
    }

    override fun onStart() {
        super.onStart()
        RelayConnection.listener = this
        RelayConnection.retryNowIfOffline()
        refreshAll()
        // The session may have started while this screen was in the background.
        if (RelayConnection.viewerPartner.isNotEmpty() && !RemoteViewerActivity.isOpen) openViewer()
    }

    override fun onDestroy() {
        handler.removeCallbacksAndMessages(null)
        sheet.dismiss()
        if (RelayConnection.listener === this) RelayConnection.listener = null
        // Leaving the app closes the connection; a configuration change does not.
        if (isFinishing && !isChangingConfigurations) RelayConnection.stop()
        super.onDestroy()
    }

    // ---- Window ----

    /** Edge to edge: the bars draw over the app's own backgrounds; the content keeps clear of them and of the keyboard. */
    private fun applyInsets() {
        val root = findViewById<View>(R.id.root_main)
        val nav = findViewById<View>(R.id.nav_bar)
        val navTop = nav.paddingTop
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            val imeVisible = insets.isVisible(WindowInsetsCompat.Type.ime())
            root.setPadding(bars.left, 0, bars.right, 0)
            if (isTablet) {
                nav.setPadding(nav.paddingLeft, navTop + bars.top, nav.paddingRight, bars.bottom)
                content.setPadding(0, bars.top, 0, if (imeVisible) ime.bottom else bars.bottom)
            } else {
                content.setPadding(0, bars.top, 0, 0)
                // While typing the bottom bar steps aside and the page sits right above the keyboard.
                nav.visibility = if (imeVisible) View.GONE else View.VISIBLE
                root.setPadding(bars.left, 0, bars.right, if (imeVisible) ime.bottom else 0)
                nav.setPadding(nav.paddingLeft, navTop, nav.paddingRight, bars.bottom + Ui.dp(this, 4f))
            }
            WindowInsetsCompat.CONSUMED
        }
    }

    // ---- Navigation ----

    private fun setupNav() {
        navItem(R.id.nav_devices, R.drawable.ic_desktop, R.string.tab_devices) { showPage(PAGE_DEVICES) }
        navItem(R.id.nav_sessions, R.drawable.ic_history, R.string.tab_sessions) { showPage(PAGE_SESSIONS) }
        navItem(R.id.nav_settings, R.drawable.ic_gear, R.string.tab_settings) { showPage(PAGE_SETTINGS) }
    }

    private fun navItem(id: Int, icon: Int, label: Int, onClick: () -> Unit) {
        val item = findViewById<View>(id)
        item.findViewById<ImageView>(R.id.nav_icon).setImageResource(icon)
        item.findViewById<TextView>(R.id.nav_label).setText(label)
        item.setOnClickListener { onClick() }
    }

    private fun updateNav() {
        val tab = when (page) {
            PAGE_SESSIONS -> R.id.nav_sessions
            PAGE_SETTINGS, PAGE_RELAY, PAGE_SECURITY -> R.id.nav_settings
            else -> R.id.nav_devices
        }
        for (id in intArrayOf(R.id.nav_devices, R.id.nav_sessions, R.id.nav_settings)) {
            val item = findViewById<View>(id)
            val on = id == tab
            item.findViewById<View>(R.id.nav_pill).setBackgroundResource(if (on) R.drawable.nav_pill else 0)
            item.findViewById<ImageView>(R.id.nav_icon).setColorFilter(Ui.color(this, if (on) R.color.text else R.color.text3))
            item.findViewById<TextView>(R.id.nav_label).apply {
                setTextColor(Ui.color(this@MainActivity, if (on) R.color.text else R.color.text3))
                typeface = androidx.core.content.res.ResourcesCompat.getFont(context, if (on) R.font.inter_semibold else R.font.inter_regular)
            }
            item.isSelected = on
        }
    }

    private fun showPage(next: Int) {
        page = next
        content.removeAllViews()
        devices = null
        sessions = null
        settingsPage = null
        relayPage = null
        securityPage = null
        val view = when (next) {
            PAGE_SESSIONS -> SessionsPage(this, content).also { sessions = it }.view
            PAGE_SETTINGS -> SettingsPage(this, content).also { settingsPage = it }.view
            PAGE_RELAY -> RelayPage(this, content).also { relayPage = it }.view
            PAGE_SECURITY -> SecurityPage(this, this, content).also { securityPage = it }.view
            else -> DevicesPage(this, content).also { page ->
                devices = page
                // Tablet: keep the computer of the last attempt in the detail pane.
                if (isTablet) target?.let { page.select(it.cleanId) }
            }.view
        }
        content.addView(view)
        updateNav()
    }

    override fun showSessions() = showPage(PAGE_SESSIONS)
    override fun showRelay() = showPage(PAGE_RELAY)
    override fun showSecurity() = showPage(PAGE_SECURITY)
    override fun showSettings() = showPage(PAGE_SETTINGS)

    private fun refreshAll() {
        devices?.refresh()
        sessions?.refresh()
        settingsPage?.refresh()
        relayPage?.refresh()
        securityPage?.refresh()
        sheet.refresh()
    }

    // ---- Relay ----

    private fun connectRelay() {
        val url = settings.gatewayUrl
        if (url.isNotEmpty() && !SettingsPage.permitted(url)) {
            toast(R.string.relay_plaintext_refused)
            return
        }
        RelayConnection.start(this, settings)
    }

    override fun restartRelay() {
        connectRelay()
        refreshAll()
    }

    // ---- Connecting ----

    override fun open(item: DeviceItem) {
        if (target?.cleanId != item.cleanId && connectState != ConnectState.Connecting) {
            target = item
            connectState = ConnectState.Ready
        }
        if (isTablet) {
            if (page != PAGE_DEVICES) showPage(PAGE_DEVICES)
            devices?.select(item.cleanId)
        } else {
            sheet.show(item)
        }
    }

    override fun showMenu(anchor: View, item: DeviceItem, preview: View) {
        DeviceMenu.show(
            this, anchor, preview,
            listOf(
                DeviceMenu.Action(getString(R.string.menu_connect), R.drawable.ic_desktop) {
                    open(item)
                    connect(item)
                },
                DeviceMenu.Action(getString(R.string.menu_rename), R.drawable.ic_pencil) { rename(item) },
                DeviceMenu.Action(getString(R.string.menu_delete), R.drawable.ic_trash, destructive = true) { delete(item) },
            ),
        )
    }

    /** Admin list: the relay's alias, seen by every administrator. Otherwise a name kept on this phone. */
    private fun rename(item: DeviceItem) {
        val shared = directory.fromRelay
        Alerts.input(
            this,
            getString(R.string.rename_title),
            getString(if (shared) R.string.rename_body_admin else R.string.rename_body_local),
            item.name.takeIf { it != getString(R.string.unknown_computer) }.orEmpty(),
            getString(R.string.rename_hint),
            getString(R.string.save),
        ) { name ->
            if (shared) {
                // The relay answers with a new host-list (onHosts).
                if (!RelayConnection.renameHost(item.cleanId, name)) toast(R.string.relay_offline)
            } else {
                directory.setAlias(item.cleanId, name)
            }
            if (target?.cleanId == item.cleanId) target = directory.lookup(item.cleanId)
            refreshAll()
        }
    }

    private fun delete(item: DeviceItem) {
        Alerts.show(
            this,
            getString(R.string.delete_title, item.name),
            getString(R.string.delete_body),
            getString(R.string.menu_delete),
            getString(R.string.cancel),
            onPrimary = {
                directory.delete(item.cleanId)
                if (target?.cleanId == item.cleanId && connectState != ConnectState.Connecting) target = null
                refreshAll()
            },
        )
    }

    override fun connect(item: DeviceItem) {
        if (connectState == ConnectState.Connecting) return
        directory.unhide(item.cleanId)
        if (RelayConnection.viewerPartner.isNotEmpty()) {
            openViewer()
            return
        }
        target = item
        if (!RelayConnection.connectTo(item.cleanId)) {
            failConnect(getString(R.string.relay_offline), record = false)
            return
        }
        connectState = ConnectState.Connecting
        handler.removeCallbacks(connectTimeout)
        handler.postDelayed(connectTimeout, CONNECT_TIMEOUT_MS)
        refreshConnect()
    }

    override fun cancelConnect() {
        if (connectState != ConnectState.Connecting) return
        handler.removeCallbacks(connectTimeout)
        connectState = ConnectState.Ready
        refreshConnect()
    }

    private fun failConnect(message: String, record: Boolean = true) {
        handler.removeCallbacks(connectTimeout)
        val item = target
        if (record && item != null) {
            history.add(SessionHistory.Record(item.cleanId, item.name, item.platform, System.currentTimeMillis(), 0, failed = true))
        }
        connectState = ConnectState.Failed(message)
        refreshConnect()
        sessions?.refresh()
    }

    private fun refreshConnect() {
        sheet.refresh()
        devices?.refresh()
    }

    private fun openViewer() {
        startActivity(Intent(this, RemoteViewerActivity::class.java))
    }

    // ---- RelayConnection.Listener ----

    override fun onRelayStatus(online: Boolean) {
        if (online) {
            onlineSinceMs = SystemClock.uptimeMillis()
            handler.removeCallbacks(registryWait)
            handler.postDelayed(registryWait, DeviceDirectory.REGISTRY_WAIT_MS + 100L)
        }
        // A request sent before the connection dropped will never be answered.
        if (!online && connectState == ConnectState.Connecting) failConnect(getString(R.string.connection_lost), record = false)
        refreshAll()
    }

    override fun onHosts() {
        devices?.refresh()
        sheet.refresh()
        securityPage?.refresh()
    }

    override fun onViewerConnected(partnerClean: String) {
        handler.removeCallbacks(connectTimeout)
        val clean = DeskId.clean(partnerClean)
        val item = target?.takeIf { it.cleanId == clean } ?: directory.lookup(clean)
        history.add(
            SessionHistory.Record(
                clean,
                item.name.takeIf { it != getString(R.string.unknown_computer) }.orEmpty(),
                item.platform,
                System.currentTimeMillis(),
                0,
                failed = false,
            ),
        )
        target = item
        connectState = ConnectState.Ready
        sheet.dismiss()
        refreshAll()
        openViewer()
    }

    override fun onSessionClosed(message: String?) {
        if (connectState == ConnectState.Connecting) {
            failConnect(message ?: getString(R.string.connect_failed_body))
            return
        }
        refreshAll()
        Toast.makeText(this, message ?: getString(R.string.session_closed), Toast.LENGTH_SHORT).show()
    }

    override fun onError(message: String?) {
        if (connectState == ConnectState.Connecting) {
            failConnect(message ?: getString(R.string.connect_failed_body))
            return
        }
        Toast.makeText(this, message ?: getString(R.string.error_generic), Toast.LENGTH_SHORT).show()
    }

    override fun onNotice(text: String) {
        Toast.makeText(this, text, Toast.LENGTH_SHORT).show()
    }

    override fun onSecurityAlert(hostClean: String, message: String) {
        handler.removeCallbacks(connectTimeout)
        connectState = ConnectState.Ready
        sheet.dismiss()
        refreshAll()
        Alerts.show(
            this,
            getString(R.string.security_alert_title),
            message,
            getString(R.string.trust_and_forget),
            getString(R.string.close),
            onPrimary = {
                RelayConnection.trustHost(hostClean)
                toast(R.string.host_trusted)
                refreshAll()
            },
        )
    }

    private fun toast(resId: Int) = Toast.makeText(this, resId, Toast.LENGTH_SHORT).show()

    private companion object {
        const val STATE_PAGE = "page"
        const val PAGE_DEVICES = 0
        const val PAGE_SESSIONS = 1
        const val PAGE_SETTINGS = 2
        const val PAGE_RELAY = 3
        const val PAGE_SECURITY = 4

        /** A connect request the relay never answers (lost message, dead host) fails after this. */
        const val CONNECT_TIMEOUT_MS = 20_000L
    }
}
