package com.opendesk.app.ui.main

import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import com.opendesk.app.R
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.DeviceItem
import com.opendesk.app.domain.Edition
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Alerts
import com.opendesk.app.ui.devices.ConnectSheet
import com.opendesk.app.ui.devices.DeviceActions
import com.opendesk.app.ui.devices.DeviceDirectory
import com.opendesk.app.ui.devices.DevicesPage
import com.opendesk.app.ui.monitor.MonitorWallActivity
import com.opendesk.app.ui.sessions.SessionsPage
import com.opendesk.app.ui.settings.RelayPage
import com.opendesk.app.ui.settings.SecurityPage
import com.opendesk.app.ui.settings.SettingsPage
import com.opendesk.app.ui.viewer.RemoteViewerActivity

/**
 * Home screen: Devices, Sessions and Settings (with its Relay and Security pages). Phones get a bottom bar and a
 * connect sheet; tablets (sw600dp) a navigation rail and a list beside the selected computer.
 *
 * This class wires the parts together: [NavBar] and [Page] (navigation), [EdgeToEdge] (window insets),
 * [ConnectFlow] (the connection attempt), [DeviceActions] (long-press menu) and the pages, which reach it only
 * through [MainHost]. It also receives the relay's events ([RelayConnection.Listener]).
 */
class MainActivity : AppCompatActivity(), RelayConnection.Listener, MainHost {

    override lateinit var settings: SettingsRepository
    override lateinit var history: SessionHistory
    override lateinit var directory: DeviceDirectory
    override val isTablet: Boolean get() = resources.getBoolean(R.bool.is_tablet)

    override val target: DeviceItem? get() = flow.target
    override val connectState: ConnectState get() = flow.state
    override var onlineSinceMs: Long = 0L
        private set

    private lateinit var content: ViewGroup
    private lateinit var sheet: ConnectSheet
    private lateinit var nav: NavBar
    private lateinit var flow: ConnectFlow
    private lateinit var actions: DeviceActions
    private var page = Page.DEVICES

    private var devices: DevicesPage? = null
    private var sessions: SessionsPage? = null
    private var settingsPage: SettingsPage? = null
    private var relayPage: RelayPage? = null
    private var securityPage: SecurityPage? = null

    private val handler = Handler(Looper.getMainLooper())

    /** The monitor wall closes with the computer to connect to when a tile was double-tapped. */
    private val monitorWall = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val id = result.data?.getStringExtra(MonitorWallActivity.EXTRA_CONNECT) ?: return@registerForActivityResult
        if (page != Page.DEVICES) showPage(Page.DEVICES)
        val item = directory.lookup(id)
        open(item)
        connect(item)
    }
    private val registryWait = Runnable { devices?.refresh() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        settings = SettingsRepository(this)
        history = SessionHistory(this)
        directory = DeviceDirectory(this, settings, history)
        flow = ConnectFlow(this, history, directory) {
            sheet.refresh()
            devices?.refresh()
            sessions?.refresh()
        }
        actions = DeviceActions(
            this, this,
            onRenamed = { flow.renamed(it.cleanId); refreshAll() },
            onDeleted = { flow.deleted(it.cleanId); refreshAll() },
        )
        sheet = ConnectSheet(this, this)
        content = findViewById(R.id.content)
        nav = NavBar(this, ::showPage)
        EdgeToEdge.apply(this, isTablet)

        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                val parent = page.parent
                if (parent != null) {
                    showPage(parent)
                } else {
                    isEnabled = false
                    onBackPressedDispatcher.onBackPressed()
                }
            }
        })

        RelayConnection.listener = this
        // The connection is process-wide: a recreated activity (rotation, theme change) keeps the running one.
        if (!RelayConnection.isStarted) connectRelay()
        // First start without a relay: nothing to connect to until one is set.
        val first = if (settings.gatewayUrl.isEmpty() || !SettingsPage.permitted(settings.gatewayUrl)) Page.RELAY else Page.DEVICES
        val saved = savedInstanceState?.getString(STATE_PAGE)?.let { name -> Page.entries.firstOrNull { it.name == name } }
        showPage(saved ?: first)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString(STATE_PAGE, page.name)
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
        flow.dispose()
        sheet.dismiss()
        if (RelayConnection.listener === this) RelayConnection.listener = null
        // Leaving the app closes the connection; a configuration change does not.
        if (isFinishing && !isChangingConfigurations) RelayConnection.stop()
        super.onDestroy()
    }

    // ---- Pages ----

    private fun showPage(next: Page) {
        page = next
        content.removeAllViews()
        devices = null
        sessions = null
        settingsPage = null
        relayPage = null
        securityPage = null
        val view = when (next) {
            Page.SESSIONS -> SessionsPage(this, content).also { sessions = it }.view
            Page.SETTINGS -> SettingsPage(this, content).also { settingsPage = it }.view
            Page.RELAY -> RelayPage(this, content).also { relayPage = it }.view
            Page.SECURITY -> SecurityPage(this, this, content).also { securityPage = it }.view
            Page.DEVICES -> DevicesPage(this, content).also { page ->
                devices = page
                // Tablet: keep the computer of the last attempt in the detail pane.
                if (isTablet) target?.let { page.select(it.cleanId) }
            }.view
        }
        content.addView(view)
        nav.select(next)
    }

    override fun showSessions() = showPage(Page.SESSIONS)
    override fun showRelay() = showPage(Page.RELAY)
    override fun showSecurity() = showPage(Page.SECURITY)
    override fun showSettings() = showPage(Page.SETTINGS)

    override fun showMonitorWall() {
        if (Edition.isAdmin) monitorWall.launch(Intent(this, MonitorWallActivity::class.java))
    }

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
            toast(getString(R.string.relay_plaintext_refused))
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
        flow.focus(item)
        if (isTablet) {
            if (page != Page.DEVICES) showPage(Page.DEVICES)
            devices?.select(item.cleanId)
        } else {
            sheet.show(item)
        }
    }

    override fun showMenu(anchor: View, item: DeviceItem, preview: View) = actions.showMenu(anchor, item, preview)

    override fun connect(item: DeviceItem) {
        if (flow.connecting) return
        directory.unhide(item.cleanId)
        if (RelayConnection.viewerPartner.isNotEmpty()) {
            openViewer()
            return
        }
        flow.start(item)
    }

    override fun cancelConnect() = flow.cancel()

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
        if (!online && flow.connecting) flow.fail(getString(R.string.connection_lost), record = false)
        refreshAll()
    }

    override fun onHosts() {
        devices?.refresh()
        sheet.refresh()
        securityPage?.refresh()
    }

    override fun onViewerConnected(partnerClean: String) {
        flow.connected(partnerClean)
        sheet.dismiss()
        refreshAll()
        openViewer()
    }

    override fun onSessionClosed(message: String?) {
        if (flow.connecting) {
            flow.fail(message ?: getString(R.string.connect_failed_body))
            return
        }
        refreshAll()
        toast(message ?: getString(R.string.session_closed))
    }

    override fun onError(message: String?) {
        if (flow.connecting) {
            flow.fail(message ?: getString(R.string.connect_failed_body))
            return
        }
        toast(message ?: getString(R.string.error_generic))
    }

    override fun onNotice(text: String) = toast(text)

    override fun onSecurityAlert(hostClean: String, message: String) {
        flow.reset()
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
                toast(getString(R.string.host_trusted))
                refreshAll()
            },
        )
    }

    private fun toast(text: String) = Toast.makeText(this, text, Toast.LENGTH_SHORT).show()

    private companion object {
        const val STATE_PAGE = "page"
    }
}
