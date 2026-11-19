package com.opendesk.app.ui.viewer

import android.Manifest
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.Bitmap
import android.os.Bundle
import android.view.View
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

    private val state = ViewerState()
    private val input = RemoteInput()
    private val decoder = FrameDecoder(::showFrame, input::requestKeyFrame)
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
        renderer = ScreenRenderer(findViewById(R.id.iv_remote_screen), findViewById(R.id.view_virtual_cursor), input)
        overlay = ControlsOverlay(findViewById(R.id.overlay_host), state, this)

        val partner = RelayConnection.viewerPartner
        partnerClean = partner
        overlay.partnerName = SessionHistory(this).records().firstOrNull { it.cleanId == partner }?.name.orEmpty()
            .ifEmpty { DeskId.format(partner) }

        setupWindow()
        retainedFrame?.let { showFrame(it, 0) }
        overlay.build()
        keyboard.onKey = input::key
        keyboard.onText = input::text
        TouchController(findViewById(R.id.iv_remote_screen), state, input, renderer, ::setLeftHeld).attach()
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (state.controlsOpen) overlay.setOpen(false) else finish()
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
            Tool.KEYBOARD -> {
                overlay.setOpen(false)
                keyboard.setKeyboardVisible(true)
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

    private fun setupWindow() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, root).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        // Keep the controls clear of the notch, of bars the user swiped in, and of the soft keyboard; the frame
        // shrinks above the keyboard so the typed text stays visible.
        ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime(),
            )
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
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
