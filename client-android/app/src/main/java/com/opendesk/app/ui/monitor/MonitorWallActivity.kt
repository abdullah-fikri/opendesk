package com.opendesk.app.ui.monitor

import android.content.Intent
import android.graphics.Bitmap
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.View
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.opendesk.app.R
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.MonitorSettings
import com.opendesk.app.domain.MonitorState
import com.opendesk.app.domain.MonitorWall
import com.opendesk.app.domain.Rotation
import com.opendesk.app.domain.WallHost
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.devices.DeviceDirectory

/**
 * The monitor wall (admin edition, see docs/features/monitor-wall.md at the repository root): live thumbnails of
 * many computers at once, view only, paged, with optional automatic page rotation.
 *
 * Only the visible page is watched, and only while this screen is in the foreground: [onStop] sends an empty
 * watch list, so a phone in a pocket costs the hosts and the mobile data nothing. A double tap on a live tile
 * closes the wall and asks the main screen to connect ([EXTRA_CONNECT]).
 */
class MonitorWallActivity : AppCompatActivity(), RelayConnection.MonitorListener {

    private lateinit var settings: SettingsRepository
    private lateinit var directory: DeviceDirectory
    private lateinit var wall: MonitorSettings
    private lateinit var grid: MonitorGrid
    private lateinit var bar: View
    private lateinit var banner: TextView
    private lateinit var empty: TextView
    private lateinit var hint: View
    private lateinit var pageLabel: TextView
    private lateinit var exitFull: View

    private val handler = Handler(Looper.getMainLooper())
    private val rotation = Rotation(SystemClock.uptimeMillis())
    private var decoder: ThumbnailDecoder? = null
    private var sheet: WallSettingsSheet? = null
    private var started = false
    private var fullscreen = false
    private var page = 0

    /** Every host on the wall, and the ones on the visible page. */
    private var hosts: List<WallHost> = emptyList()
    private var shown: List<WallHost> = emptyList()
    private val states = HashMap<String, String>()
    private val pictures = HashMap<String, Bitmap>()

    private val pages: Int get() = MonitorWall.pageCount(hosts.size, MonitorWall.perPage(wall))

    private val tick = object : Runnable {
        override fun run() {
            if (rotation.due(MonitorWall.rotateIntervalMs(wall), pages, SystemClock.uptimeMillis())) turn(1, manual = false)
            handler.postDelayed(this, TICK_MS)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_monitor)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        settings = SettingsRepository(this)
        directory = DeviceDirectory(this, settings, SessionHistory(this))
        wall = settings.monitor(MonitorWall.MAX_GRID)
        page = savedInstanceState?.getInt(STATE_PAGE) ?: 0

        bar = findViewById(R.id.wall_bar)
        banner = findViewById(R.id.wall_banner)
        empty = findViewById(R.id.wall_empty)
        hint = findViewById(R.id.wall_hint)
        pageLabel = findViewById(R.id.wall_page)
        exitFull = findViewById(R.id.wall_exit_full)
        grid = MonitorGrid(this).apply {
            onSwipe = { turn(it, manual = true) }
            onDoubleTap = ::openTile
        }
        findViewById<FrameLayout>(R.id.wall_grid).addView(grid, 0, FrameLayout.LayoutParams(-1, -1))

        findViewById<View>(R.id.wall_back).setOnClickListener { finish() }
        findViewById<View>(R.id.wall_prev).setOnClickListener { turn(-1, manual = true) }
        findViewById<View>(R.id.wall_next).setOnClickListener { turn(1, manual = true) }
        findViewById<View>(R.id.wall_settings).setOnClickListener { showSettings() }
        findViewById<View>(R.id.wall_full).setOnClickListener { setFullscreen(true) }
        exitFull.setOnClickListener { setFullscreen(false) }
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (fullscreen) setFullscreen(false) else finish()
            }
        })
        immersive()
        setFullscreen(savedInstanceState?.getBoolean(STATE_FULLSCREEN) == true)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putInt(STATE_PAGE, page)
        outState.putBoolean(STATE_FULLSCREEN, fullscreen)
    }

    override fun onStart() {
        super.onStart()
        started = true
        RelayConnection.monitorListener = this
        val decoder = ThumbnailDecoder(::onPictures).also { decoder = it }
        RelayConnection.thumbnailSink = decoder::offer
        RelayConnection.retryNowIfOffline()
        rotation.reset(SystemClock.uptimeMillis())
        rebuild()
        handler.postDelayed(tick, TICK_MS)
    }

    override fun onStop() {
        started = false
        handler.removeCallbacks(tick)
        // Nobody looks at the wall now: the relay stops the hosts' thumbnails.
        RelayConnection.watchMonitors(emptyList())
        RelayConnection.thumbnailSink = null
        if (RelayConnection.monitorListener === this) RelayConnection.monitorListener = null
        decoder?.close()
        decoder = null
        states.clear()
        pictures.clear()
        super.onStop()
    }

    override fun onDestroy() {
        sheet?.dismiss()
        super.onDestroy()
    }

    // ---- What the wall shows ----

    /** Re-reads the hosts and the settings, picks the page, and watches exactly the hosts on it. */
    private fun rebuild() {
        hosts = if (directory.fromRelay) {
            MonitorWall.wallHosts(directory.all().map { WallHost(it.cleanId, it.name, it.online == true) }, wall)
        } else {
            emptyList()
        }
        val per = MonitorWall.perPage(wall)
        page = page.coerceIn(0, pages - 1)
        shown = MonitorWall.pageOf(hosts, per, page)
        grid.slots = per
        grid.columns = MonitorWall.grid(wall)
        val ids = shown.map { it.id }.toSet()
        states.keys.retainAll(ids)
        pictures.keys.retainAll(ids)
        if (started) RelayConnection.watchMonitors(shown.map { it.id })
        render()
    }

    private fun render() {
        val online = RelayConnection.online
        pageLabel.text = getString(R.string.wall_page, page + 1, pages)
        listOf(R.id.wall_prev, R.id.wall_next).forEach { findViewById<View>(it).isEnabled = pages > 1 }
        listOf(R.id.wall_prev, R.id.wall_next, R.id.wall_page).forEach { findViewById<View>(it).alpha = if (pages > 1) 1f else 0.35f }

        val notice = when {
            !online -> getString(R.string.wall_relay_offline)
            !RelayConnection.relayHasMonitor -> getString(R.string.wall_relay_old)
            states.values.any { it == MonitorState.DENIED } -> getString(R.string.wall_denied)
            onMobileData() && shown.isNotEmpty() -> getString(R.string.wall_cellular)
            else -> null
        }
        banner.text = notice
        banner.visibility = if (notice != null && !fullscreen) View.VISIBLE else View.GONE

        val emptyText = when {
            shown.isNotEmpty() -> null
            !directory.fromRelay -> if (online) getString(R.string.wall_loading) else null
            wall.all -> getString(R.string.wall_none_online)
            else -> getString(R.string.wall_none_selected)
        }
        empty.text = emptyText
        empty.visibility = if (emptyText != null) View.VISIBLE else View.GONE

        grid.tiles.forEachIndexed { i, tile ->
            val host = shown.getOrNull(i)
            if (host == null) {
                tile.visibility = View.INVISIBLE
                return@forEachIndexed
            }
            tile.visibility = View.VISIBLE
            val state = states[host.id]
            tile.bind(host.name, state == MonitorState.LIVE, pictures[host.id], statusText(state))
        }
    }

    private fun statusText(state: String?): String = getString(
        when (state) {
            MonitorState.LIVE -> R.string.tile_waiting
            MonitorState.OFFLINE -> R.string.tile_offline
            MonitorState.UNSUPPORTED -> R.string.tile_unsupported
            MonitorState.DENIED -> R.string.tile_denied
            else -> R.string.tile_connecting
        },
    )

    private fun onPictures(decoded: Map<String, Bitmap>) {
        if (!started) return
        val ids = shown.map { it.id }.toSet()
        decoded.forEach { (id, bitmap) ->
            if (id in ids) {
                pictures[id] = bitmap
                states[id] = MonitorState.LIVE
            }
        }
        render()
    }

    private fun turn(delta: Int, manual: Boolean) {
        if (pages <= 1) return
        page = MonitorWall.stepPage(page, delta, pages)
        if (manual) rotation.reset(SystemClock.uptimeMillis())
        rebuild()
    }

    private fun openTile(index: Int) {
        val host = shown.getOrNull(index) ?: return
        if (states[host.id] != MonitorState.LIVE) return
        setResult(RESULT_OK, Intent().putExtra(EXTRA_CONNECT, host.id))
        finish()
    }

    private fun showSettings() {
        val all = if (directory.fromRelay) directory.all().map { WallHost(it.cleanId, it.name, it.online == true) } else emptyList()
        val choices = (1..MonitorWall.MAX_GRID).toList()
        sheet?.dismiss()
        sheet = WallSettingsSheet(this, wall, choices, all) { next ->
            wall = next
            settings.saveMonitor(next)
            rebuild()
        }.also { it.show() }
    }

    // ---- Window ----

    private fun setFullscreen(on: Boolean) {
        fullscreen = on
        bar.visibility = if (on) View.GONE else View.VISIBLE
        hint.visibility = if (on) View.GONE else View.VISIBLE
        exitFull.visibility = if (on) View.VISIBLE else View.GONE
        render()
    }

    /** Hides the system bars like the viewer; content stays clear of cutouts and bars swiped in. */
    private fun immersive() {
        val root = findViewById<View>(R.id.wall_root)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, root).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            WindowInsetsCompat.CONSUMED
        }
    }

    private fun onMobileData(): Boolean {
        val manager = getSystemService(ConnectivityManager::class.java) ?: return false
        val caps = manager.getNetworkCapabilities(manager.activeNetwork) ?: return false
        return caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)
    }

    // ---- RelayConnection.MonitorListener ----

    override fun onMonitorRelay(online: Boolean) {
        // A new connection starts with no watches; RelayConnection sends the list again and the relay answers.
        states.clear()
        if (!online) pictures.clear()
        render()
    }

    override fun onMonitorState(hostClean: String, state: String) {
        if (shown.none { it.id == hostClean }) return
        states[hostClean] = state
        if (state != MonitorState.LIVE) pictures.remove(hostClean)
        render()
    }

    override fun onMonitorHosts() = rebuild()

    companion object {
        /** Result extra: the clean ID of the computer to connect to. */
        const val EXTRA_CONNECT = "connect"
        private const val STATE_PAGE = "page"
        private const val STATE_FULLSCREEN = "fullscreen"
        private const val TICK_MS = 1_000L
    }
}
