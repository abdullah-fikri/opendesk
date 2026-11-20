package com.opendesk.app.ui.viewer

import android.Manifest
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.SurfaceTexture
import android.os.Build
import android.os.Bundle
import android.view.Surface
import android.view.TextureView
import android.view.View
import android.view.WindowManager
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.opendesk.app.R
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.WindowsKeys
import com.opendesk.app.platform.audio.VoiceCallManager
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Alerts
import com.opendesk.app.ui.viewer.ControlsOverlay.Tool
import com.opendesk.app.ui.viewer.RemoteInput.Companion.BUTTON_LEFT
import com.opendesk.app.ui.viewer.RemoteInput.Companion.BUTTON_RIGHT

/**
 * Full-screen view of a computer this phone controls: the session's lifecycle, the window and the wiring between
 * its parts — [FrameDecoder] (video), [ScreenRenderer] (frame, zoom, cursor), [TouchController] (gestures),
 * [ControlsOverlay] (controls) and [RemoteInput] (what goes to the computer).
 *
 * Video is applied on a worker thread and only the newest frame is drawn, so a slow phone never builds up latency. The
 * activity handles rotation itself (see the manifest), so the session, zoom and last frame survive it; the controls
 * overlay is inflated again so portrait gets the bottom sheet and landscape the side panel.
 */
class RemoteViewerActivity : AppCompatActivity(), RelayConnection.ViewerListener, ControlsOverlay.Listener {

    companion object {
        /** Whether a viewer screen exists (the main screen opens one when a session starts in the background). */
        @Volatile
        var isOpen = false
            private set

        /** Last frame shown, so a recreated screen does not wait for the host's next change. */
        private var retainedFrame: Bitmap? = null
    }

    private lateinit var root: View
    private lateinit var connectingOverlay: View
    private lateinit var keyboard: RemoteKeyboardView
    private lateinit var settings: SettingsRepository
    private lateinit var renderer: ScreenRenderer
    private lateinit var overlay: ControlsOverlay
    private lateinit var overlayHost: android.view.ViewGroup

    private val state = ViewerState()
    private val input = RemoteInput()
    private val h264 = H264Decoder(::onH264Failed)
    private val decoder = FrameDecoder(::showFrame, input::requestKeyFrame, h264, ::showVideo)
    private var videoSurface: Surface? = null
    private var partnerClean = ""

    private val micPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            VoiceCallManager.setMicMuted(false)
            overlay.refresh()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Restored after the process died: the session is gone with it.
        if (RelayConnection.viewerPartner.isEmpty()) {
            retainedFrame = null
            finish()
            return
        }
        isOpen = true
        setContentView(R.layout.activity_remote)

        root = findViewById(R.id.root_remote_container)
        connectingOverlay = findViewById(R.id.layout_connecting_overlay)
        keyboard = findViewById(R.id.view_remote_keyboard)
        settings = SettingsRepository(this)
        val video = findViewById<TextureView>(R.id.tv_remote_video)
        renderer = ScreenRenderer(findViewById(R.id.iv_remote_screen), video, findViewById(R.id.view_virtual_cursor), input)
        video.surfaceTextureListener = VideoSurface()
        overlayHost = findViewById(R.id.overlay_host)
        overlay = ControlsOverlay(overlayHost, state, this)

        val partner = RelayConnection.viewerPartner
        partnerClean = partner
        overlay.partnerName = SessionHistory(this).records().firstOrNull { it.cleanId == partner }?.name.orEmpty()
            .ifEmpty { DeskId.format(partner) }

        setupWindow()
        applyFill()
        retainedFrame?.let { showFrame(it, 0) }
        overlay.build()
        keyboard.onKey = input::key
        keyboard.onText = input::text
        TouchController(findViewById(R.id.iv_remote_screen), state, input, renderer, ::setLeftHeld, overlay::showPill).attach()
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                when {
                    overlay.closeRadial() -> Unit
                    state.controlsOpen -> overlay.setOpen(false)
                    else -> finish()
                }
            }
        })
        if (!settings.gesturesSeen) {
            settings.gesturesSeen = true
            root.post { onGestures() }
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        // Portrait uses the bottom sheet, landscape the side panel (layout-land/view_controls.xml).
        applyFill()
        overlay.build()
    }

    override fun onStart() {
        super.onStart()
        if (RelayConnection.viewerPartner.isEmpty()) {
            finish()
            return
        }
        RelayConnection.viewerListener = this
        RelayConnection.videoSink = decoder::offer
        decoder.start()
    }

    override fun onStop() {
        // In the background: stop decoding, release a held button and the microphone (Android blocks background
        // capture anyway). The session itself stays up until the user ends it.
        RelayConnection.videoSink = null
        decoder.stop()
        h264.release()
        if (state.leftHeld) setLeftHeld(false)
        if (!VoiceCallManager.isMuted) {
            VoiceCallManager.setMicMuted(true)
            overlay.refresh()
        }
        if (RelayConnection.viewerListener === this) RelayConnection.viewerListener = null
        super.onStop()
    }

    override fun onPause() {
        // Ending here (not in onDestroy) means the main screen already sees no session when it resumes.
        if (isFinishing) leave()
        super.onPause()
    }

    override fun onDestroy() {
        if (isFinishing) leave()
        super.onDestroy()
    }

    /** Leaving the screen ends the session, whichever way it was left. Idempotent. */
    private fun leave() {
        // The Sessions list shows how long it ran (once, however the screen was left).
        if (partnerClean.isNotEmpty()) SessionHistory(this).finish(partnerClean, System.currentTimeMillis())
        partnerClean = ""
        RelayConnection.endViewing()
        h264.close()
        retainedFrame = null
        isOpen = false
    }

    // ---- RelayConnection.ViewerListener ----

    override fun onViewerSessionEnded(message: String?) {
        finish()
    }

    override fun onMicMuted() = overlay.refresh()

    override fun onEndToEnd() = overlay.refreshStatus()

    // ---- ControlsOverlay.Listener ----

    override fun onExit() = finish()

    override fun onGestures() {
        Alerts.table(
            this,
            getString(R.string.gestures_title),
            listOf(
                getString(R.string.gesture_tap) to getString(R.string.gesture_tap_does),
                getString(R.string.gesture_double) to getString(R.string.gesture_double_does),
                getString(R.string.gesture_long) to getString(R.string.gesture_long_does),
                getString(R.string.gesture_two_drag) to getString(R.string.gesture_two_drag_does),
                getString(R.string.gesture_pinch) to getString(R.string.gesture_pinch_does),
                getString(R.string.gesture_one_drag) to getString(R.string.gesture_one_drag_does),
                getString(R.string.gesture_edge) to getString(R.string.gesture_edge_does),
                getString(R.string.gesture_top) to getString(R.string.gesture_top_does),
            ),
            getString(R.string.got_it),
        )
    }

    override fun onTool(tool: Tool) {
        when (tool) {
            Tool.LEFT -> input.mouse("click", BUTTON_LEFT)
            Tool.DOUBLE -> input.mouse("dblclick", BUTTON_LEFT)
            Tool.RIGHT -> input.mouse("click", BUTTON_RIGHT)
            Tool.HOLD -> setLeftHeld(!state.leftHeld)
            Tool.ESC -> input.key(WindowsKeys.VK_ESCAPE)
            Tool.TAB -> input.key(WindowsKeys.VK_TAB)
            Tool.WIN -> input.winKey()
            Tool.ALT_TAB -> input.shortcut("alt-tab")
            Tool.TASK_MANAGER -> input.shortcut("taskmgr")
            Tool.MOUSE_MODE -> {
                state.trackpadMode = !state.trackpadMode
                overlay.refresh()
                Toast.makeText(this, if (state.trackpadMode) R.string.menu_trackpad else R.string.menu_touch, Toast.LENGTH_SHORT).show()
            }
            Tool.FIT_MODE -> setFill(false)
            Tool.FILL_MODE -> setFill(true)
            Tool.KEYBOARD -> {
                overlay.setOpen(false)
                keyboard.setKeyboardVisible(!state.keyboardVisible)
            }
            Tool.HIDE_KEYBOARD -> keyboard.setKeyboardVisible(false)
            Tool.SCROLL -> {
                state.scrollMode = !state.scrollMode
                if (state.scrollMode) Toast.makeText(this, R.string.scroll_mode_on, Toast.LENGTH_SHORT).show()
                overlay.refresh()
            }
            Tool.FIT -> renderer.fit()
            Tool.MIC -> toggleMic()
            Tool.MONITOR -> {
                input.switchMonitor()
                Toast.makeText(this, R.string.switching_monitor, Toast.LENGTH_SHORT).show()
            }
        }
    }

    // ---- Window ----

    private val landscape: Boolean get() = resources.configuration.orientation == Configuration.ORIENTATION_LANDSCAPE

    /** The fit/fill choice remembered for the current orientation. */
    private fun applyFill() {
        state.fill = settings.viewerFill(landscape)
        renderer.setFill(state.fill)
    }

    private fun setFill(fill: Boolean) {
        state.fill = fill
        settings.setViewerFill(landscape, fill)
        renderer.setFill(fill)
    }

    private fun setupWindow() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        // Draw into the notch area too: in fill mode the frame reaches every edge.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            window.attributes = window.attributes.apply {
                layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
        }
        WindowInsetsControllerCompat(window, root).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        // The frame uses the whole screen, notch included, and only shrinks above the soft keyboard so the typed
        // text stays visible. The controls keep clear of the notch, of bars the user swiped in, and of the keyboard.
        // Bars swiped in from the top also bring the session pill.
        ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime(),
            )
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            view.setPadding(0, 0, 0, ime.bottom)
            overlayHost.setPadding(bars.left, bars.top, bars.right, (bars.bottom - ime.bottom).coerceAtLeast(0))
            if (insets.isVisible(WindowInsetsCompat.Type.statusBars())) overlay.showPill()
            val typing = insets.isVisible(WindowInsetsCompat.Type.ime())
            if (typing != state.keyboardVisible) {
                state.keyboardVisible = typing
                overlay.refresh()
            }
            WindowInsetsCompat.CONSUMED
        }
    }

    // ---- Session controls ----

    private fun toggleMic() {
        if (!VoiceCallManager.isMuted) {
            VoiceCallManager.setMicMuted(true)
            overlay.refresh()
        } else if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            micPermission.launch(Manifest.permission.RECORD_AUDIO)
        } else {
            VoiceCallManager.setMicMuted(false)
            overlay.refresh()
        }
    }

    private fun setLeftHeld(held: Boolean) {
        state.leftHeld = held
        overlay.refresh()
        input.mouse(if (held) "mousedown" else "mouseup", BUTTON_LEFT)
    }

    /** Main thread: the hardware decoder is drawing `width` x `height` pictures onto the video layer. */
    private fun showVideo(width: Int, height: Int, fps: Int) {
        if (isDestroyed) return
        renderer.showVideo(width, height)
        if (connectingOverlay.visibility != View.GONE) connectingOverlay.visibility = View.GONE
        if (fps > 0) overlay.fps = fps
        overlay.refreshStatus()
    }

    /** Any thread: H.264 does not work on this phone; from the next connection on, hosts send JPEG only. */
    private fun onH264Failed() = runOnUiThread {
        if (settings.h264Failed) return@runOnUiThread
        settings.h264Failed = true
        Toast.makeText(this, R.string.h264_failed, Toast.LENGTH_LONG).show()
    }

    /** The video layer's surface: given to the H.264 decoder while it exists. */
    private inner class VideoSurface : TextureView.SurfaceTextureListener {
        override fun onSurfaceTextureAvailable(texture: SurfaceTexture, width: Int, height: Int) {
            videoSurface = Surface(texture).also { h264.surface = it }
            renderer.apply()
            // The host may be mid-motion: ask for an IDR to start from.
            input.requestKeyFrame()
        }

        override fun onSurfaceTextureSizeChanged(texture: SurfaceTexture, width: Int, height: Int) = renderer.apply()

        override fun onSurfaceTextureDestroyed(texture: SurfaceTexture): Boolean {
            h264.surface = null
            videoSurface?.release()
            videoSurface = null
            return true
        }

        override fun onSurfaceTextureUpdated(texture: SurfaceTexture) = renderer.onVideoPicture()
    }

    /** Main thread: shows a decoded frame. */
    private fun showFrame(bitmap: Bitmap, fps: Int) {
        if (isDestroyed) return
        renderer.show(bitmap)
        retainedFrame = bitmap
        if (connectingOverlay.visibility != View.GONE) connectingOverlay.visibility = View.GONE
        if (fps > 0) overlay.fps = fps
        overlay.refreshStatus()
    }
}
