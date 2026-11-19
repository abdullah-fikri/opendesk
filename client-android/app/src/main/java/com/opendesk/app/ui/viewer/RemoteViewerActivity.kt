package com.opendesk.app.ui.viewer

import android.Manifest
import android.annotation.SuppressLint
import android.content.pm.PackageManager
import android.content.res.ColorStateList
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Matrix
import android.os.Bundle
import android.os.SystemClock
import android.view.GestureDetector
import android.view.HapticFeedbackConstants
import android.view.LayoutInflater
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.widget.ImageView
import android.widget.TextView
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.core.content.res.ResourcesCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.core.widget.TextViewCompat
import com.opendesk.app.R
import com.opendesk.app.data.dto.InputEvent
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.Viewport
import com.opendesk.app.domain.WindowsKeys
import com.opendesk.app.platform.audio.VoiceCallManager
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Alerts
import kotlin.math.abs
import kotlin.math.hypot

/**
 * Full-screen view of a computer this phone controls.
 *
 * Touch: tap = left click, double tap = double click, long press or two-finger tap = right click, two-finger
 * vertical drag = scroll wheel, pinch = zoom (moving the fingers while pinching pans). In trackpad mode one finger
 * moves a virtual cursor and a zoomed view follows it; in touch mode the cursor jumps to the finger.
 *
 * Video is applied on a worker thread and only the newest frame is drawn, so a slow phone never builds up latency. The
 * activity handles rotation itself (see the manifest), so the session, zoom and last frame survive it; the controls
 * overlay is inflated again so portrait gets the bottom sheet and landscape the side panel.
 */
class RemoteViewerActivity : AppCompatActivity(), RelayConnection.ViewerListener {

    companion object {
        /** Whether a viewer screen exists (the main screen opens one when a session starts in the background). */
        @Volatile
        var isOpen = false
            private set

        /** Last frame shown, so a recreated screen does not wait for the host's next change. */
        private var retainedFrame: Bitmap? = null

        private const val MOVE_THROTTLE_MS = 15L
        private const val TRACKPAD_SENSITIVITY = 1.35f
        private const val SCROLL_STEP = 120
        private const val SCROLL_THRESHOLD_PX = 18f
        private const val ZOOM_SLOP_PX = 24f
        private const val TWO_FINGER_TAP_MS = 300L
        private const val CURSOR_MARGIN_PX = 48f
        private const val BUTTON_LEFT = 0
        private const val BUTTON_RIGHT = 2
        /** Video messages waiting for the decoder; more means it fell behind, and a key frame resynchronizes. */
        private const val MAX_QUEUED = 8
        private const val KEY_REQUEST_INTERVAL_MS = 500L
    }

    /** What the current two-finger gesture turned out to be. */
    private enum class TwoFinger { UNDECIDED, SCROLL, ZOOM }

    private lateinit var root: View
    private lateinit var ivScreen: ImageView
    private lateinit var virtualCursor: View
    private lateinit var connectingOverlay: View
    private lateinit var overlayHost: ViewGroup
    private lateinit var keyboard: RemoteKeyboardView
    private lateinit var settings: SettingsRepository

    // The controls overlay (view_overlay), inflated again on rotation.
    private var overlay: View? = null
    private var controlsOpen = false
    private var partnerName = ""
    private var partnerClean = ""

    private val viewport = Viewport()
    private val imageMatrix = Matrix()
    private val decoder = FrameDecoder(::showFrame)

    // Virtual cursor in normalized coordinates [0..1].
    private var cursorX = 0.5f
    private var cursorY = 0.5f
    private var trackpadMode = true
    private var leftHeld = false
    private var keyboardVisible = false

    /** One-finger drags turn the wheel instead of moving the pointer (the Scroll control). */
    private var scrollMode = false

    private var lastTouchX = 0f
    private var lastTouchY = 0f
    private var lastMoveSentMs = 0L
    /** More than one finger touched during the current gesture: ignore the remaining finger's moves. */
    private var multiTouch = false
    private var twoFingerMode = TwoFinger.UNDECIDED
    private var twoFingerDownTime = 0L
    private var twoFingerStartSpan = 0f
    private var twoFingerStartY = 0f
    private var lastSpan = 0f
    private var lastFocusX = 0f
    private var lastFocusY = 0f
    private var lastScrollY = 0f

    private val micPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            VoiceCallManager.setMicMuted(false)
            updateMicButton()
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
        ivScreen = findViewById(R.id.iv_remote_screen)
        virtualCursor = findViewById(R.id.view_virtual_cursor)
        connectingOverlay = findViewById(R.id.layout_connecting_overlay)
        overlayHost = findViewById(R.id.overlay_host)
        keyboard = findViewById(R.id.view_remote_keyboard)
        settings = SettingsRepository(this)
        val partner = RelayConnection.viewerPartner
        partnerClean = partner
        partnerName = SessionHistory(this).records().firstOrNull { it.cleanId == partner }?.name.orEmpty()
            .ifEmpty { DeskId.format(partner) }

        setupWindow()
        virtualCursor.visibility = View.VISIBLE
        ivScreen.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> layoutFrame() }
        retainedFrame?.let { showFrame(it, 0) }
        buildOverlay()
        setupKeyboard()
        setupTouch()
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (controlsOpen) showControls(false) else finish()
            }
        })
        if (!settings.gesturesSeen) {
            settings.gesturesSeen = true
            root.post { showGestures() }
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        // Portrait uses the bottom sheet, landscape the side panel (layout-land/view_controls.xml).
        buildOverlay()
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
        if (leftHeld) setLeftHeld(false)
        if (!VoiceCallManager.isMuted) {
            VoiceCallManager.setMicMuted(true)
            updateMicButton()
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

    override fun onMicMuted() = updateMicButton()

    override fun onEndToEnd() = updateStatusBadge()

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
            if (typing != keyboardVisible) {
                keyboardVisible = typing
                updateOverlay()
            }
            WindowInsetsCompat.CONSUMED
        }
    }

    // ---- Controls overlay ----

    private fun buildOverlay() {
        overlayHost.removeAllViews()
        val view = LayoutInflater.from(this).inflate(R.layout.view_overlay, overlayHost, false)
        overlayHost.addView(view)
        overlay = view
        sizeToolIcons(view)

        view.findViewById<View>(R.id.btn_exit_remote).setOnClickListener { finish() }
        view.findViewById<View>(R.id.tv_partner).setOnClickListener { showGestures() }
        view.findViewById<View>(R.id.btn_controls).apply {
            setOnClickListener { showControls(true) }
            setOnLongClickListener {
                showGestures()
                true
            }
        }
        view.findViewById<View>(R.id.controls_handle).setOnClickListener { showControls(false) }
        view.findViewById<View>(R.id.controls_title)?.setOnClickListener { showGestures() }
        view.findViewById<View>(R.id.seg_trackpad).setOnClickListener { setTrackpadMode(true) }
        view.findViewById<View>(R.id.seg_touch).setOnClickListener { setTrackpadMode(false) }

        tool(R.id.btn_left_click) { sendMouse("click", BUTTON_LEFT) }
        tool(R.id.btn_double_click) { sendMouse("dblclick", BUTTON_LEFT) }
        tool(R.id.btn_right_click, HapticFeedbackConstants.LONG_PRESS) { sendMouse("click", BUTTON_RIGHT) }
        tool(R.id.btn_hold_drag, HapticFeedbackConstants.LONG_PRESS) { setLeftHeld(!leftHeld) }
        tool(R.id.btn_key_esc) { sendKey(WindowsKeys.VK_ESCAPE) }
        tool(R.id.btn_key_tab) { sendKey(WindowsKeys.VK_TAB) }
        tool(R.id.btn_key_win) { RelayConnection.sendInput(InputEvent(action = "shortcut", name = "win")) }
        tool(R.id.btn_keyboard) {
            showControls(false)
            keyboard.setKeyboardVisible(true)
        }
        tool(R.id.btn_scroll) {
            scrollMode = !scrollMode
            if (scrollMode) Toast.makeText(this, R.string.scroll_mode_on, Toast.LENGTH_SHORT).show()
            updateOverlay()
        }
        tool(R.id.btn_fit) {
            viewport.reset()
            applyMatrix()
        }
        tool(R.id.btn_toggle_mic) { toggleMic() }
        tool(R.id.btn_switch_monitor) {
            RelayConnection.sendInput(InputEvent(action = "switch-monitor"))
            Toast.makeText(this, R.string.switching_monitor, Toast.LENGTH_SHORT).show()
        }
        tool(R.id.kb_esc) { sendKey(WindowsKeys.VK_ESCAPE) }
        tool(R.id.kb_tab) { sendKey(WindowsKeys.VK_TAB) }
        tool(R.id.kb_win) { RelayConnection.sendInput(InputEvent(action = "shortcut", name = "win")) }
        tool(R.id.kb_hide) { keyboard.setKeyboardVisible(false) }
        updateOverlay()
    }

    private fun tool(id: Int, haptic: Int = HapticFeedbackConstants.VIRTUAL_KEY, action: () -> Unit) {
        overlay?.findViewById<View>(id)?.setOnClickListener {
            it.performHapticFeedback(haptic)
            action()
        }
    }

    /** Compound icons of the control tiles at the design size (18 dp; 15 dp beside a label). */
    private fun sizeToolIcons(view: View) {
        val size = (18 * resources.displayMetrics.density).toInt()
        val small = (15 * resources.displayMetrics.density).toInt()
        fun walk(v: View) {
            if (v is ViewGroup) {
                for (i in 0 until v.childCount) walk(v.getChildAt(i))
            } else if (v is TextView) {
                val d = v.compoundDrawablesRelative
                if (d.all { it == null }) return
                val px = if (d[0] != null) small else size
                d.forEach { it?.setBounds(0, 0, px, px) }
                v.setCompoundDrawablesRelative(d[0], d[1], d[2], d[3])
            }
        }
        walk(view)
    }

    private fun showControls(open: Boolean) {
        controlsOpen = open
        updateOverlay()
    }

    private fun setTrackpadMode(trackpad: Boolean) {
        trackpadMode = trackpad
        updateOverlay()
    }

    /** Shows the overlay state: which part is visible, the mode, active tools and the session pill. */
    private fun updateOverlay() {
        val view = overlay ?: return
        val panelOpen = controlsOpen && !keyboardVisible
        view.findViewById<View>(R.id.controls_panel).visibility = if (panelOpen) View.VISIBLE else View.GONE
        view.findViewById<View>(R.id.btn_controls).visibility = if (panelOpen || keyboardVisible) View.GONE else View.VISIBLE
        view.findViewById<View>(R.id.key_bar).visibility = if (keyboardVisible) View.VISIBLE else View.GONE

        segment(view.findViewById(R.id.seg_trackpad), trackpadMode)
        segment(view.findViewById(R.id.seg_touch), !trackpadMode)
        val muted = VoiceCallManager.isMuted
        view.findViewById<TextView>(R.id.btn_toggle_mic).apply {
            setText(if (muted) R.string.mic_off else R.string.mic_on)
            active(this, !muted)
        }
        active(view.findViewById(R.id.btn_hold_drag), leftHeld)
        active(view.findViewById(R.id.btn_scroll), scrollMode)
        updateStatusBadge()
    }

    private fun segment(view: TextView, on: Boolean) {
        view.setBackgroundResource(if (on) R.drawable.seg_on else 0)
        val color = ContextCompat.getColor(this, if (on) R.color.text else R.color.text3)
        view.setTextColor(color)
        TextViewCompat.setCompoundDrawableTintList(view, ColorStateList.valueOf(color))
        view.typeface = ResourcesCompat.getFont(this, if (on) R.font.inter_semibold else R.font.inter_regular)
    }

    /** A control tile that is on (held button, microphone, scroll mode) is tinted with the accent. */
    private fun active(view: TextView, on: Boolean) {
        view.setBackgroundResource(if (on) R.drawable.tool_tile_accent else R.drawable.tool_tile)
        val color = ContextCompat.getColor(this, if (on) R.color.accent_tx else R.color.text)
        view.setTextColor(color)
        TextViewCompat.setCompoundDrawableTintList(view, ColorStateList.valueOf(color))
    }

    private fun showGestures() {
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

    private fun toggleMic() {
        if (!VoiceCallManager.isMuted) {
            VoiceCallManager.setMicMuted(true)
            updateMicButton()
        } else if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            micPermission.launch(Manifest.permission.RECORD_AUDIO)
        } else {
            VoiceCallManager.setMicMuted(false)
            updateMicButton()
        }
    }

    private fun updateMicButton() = updateOverlay()

    private fun setLeftHeld(held: Boolean) {
        leftHeld = held
        updateOverlay()
        sendMouse(if (held) "mousedown" else "mouseup", BUTTON_LEFT)
    }

    // ---- Keyboard ----

    private fun setupKeyboard() {
        keyboard.onKey = ::sendKey
        keyboard.onText = { text ->
            // A soft keyboard may commit a newline as text; the host gets a real Enter key for it.
            text.split('\n').forEachIndexed { i, part ->
                if (i > 0) sendKey(WindowsKeys.VK_RETURN)
                if (part.isNotEmpty()) RelayConnection.sendInput(InputEvent(action = "text", text = part))
            }
        }
    }

    private fun sendKey(vk: Int) {
        RelayConnection.sendInput(InputEvent(action = "keydown", keyCode = vk))
        RelayConnection.sendInput(InputEvent(action = "keyup", keyCode = vk))
    }

    // ---- Touch ----

    @SuppressLint("ClickableViewAccessibility") // Touches are forwarded to the remote machine, not clicks here.
    private fun setupTouch() {
        val gestures = GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
            override fun onSingleTapConfirmed(e: MotionEvent): Boolean {
                if (multiTouch) return true
                if (!trackpadMode) pointAt(e.x, e.y)
                if (leftHeld) setLeftHeld(false) else sendMouse("click", BUTTON_LEFT)
                return true
            }

            override fun onDoubleTap(e: MotionEvent): Boolean {
                if (multiTouch) return true
                if (!trackpadMode) pointAt(e.x, e.y)
                ivScreen.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                sendMouse("dblclick", BUTTON_LEFT)
                return true
            }

            override fun onLongPress(e: MotionEvent) {
                if (multiTouch) return
                if (!trackpadMode) pointAt(e.x, e.y)
                ivScreen.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                sendMouse("click", BUTTON_RIGHT)
                Toast.makeText(this@RemoteViewerActivity, R.string.right_click, Toast.LENGTH_SHORT).show()
            }
        })

        ivScreen.setOnTouchListener { _, event ->
            gestures.onTouchEvent(event)
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    multiTouch = false
                    lastTouchX = event.x
                    lastTouchY = event.y
                    if (!trackpadMode) {
                        pointAt(event.x, event.y)
                        sendMove(force = true)
                    }
                }
                MotionEvent.ACTION_POINTER_DOWN -> {
                    multiTouch = true
                    if (event.pointerCount == 2) startTwoFinger(event)
                }
                MotionEvent.ACTION_MOVE -> if (event.pointerCount >= 2) onTwoFingerMove(event) else if (!multiTouch) onMove(event)
                // A quick two-finger tap is a right click.
                MotionEvent.ACTION_POINTER_UP -> if (event.pointerCount == 2 && twoFingerMode == TwoFinger.UNDECIDED &&
                    SystemClock.uptimeMillis() - twoFingerDownTime < TWO_FINGER_TAP_MS
                ) {
                    ivScreen.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                    sendMouse("click", BUTTON_RIGHT)
                }
                MotionEvent.ACTION_UP -> if (!multiTouch) sendMove(force = true)
            }
            true
        }
    }

    private fun startTwoFinger(event: MotionEvent) {
        twoFingerMode = TwoFinger.UNDECIDED
        twoFingerDownTime = SystemClock.uptimeMillis()
        twoFingerStartSpan = span(event)
        lastSpan = twoFingerStartSpan
        lastFocusX = (event.getX(0) + event.getX(1)) * 0.5f
        lastFocusY = (event.getY(0) + event.getY(1)) * 0.5f
        twoFingerStartY = lastFocusY
        lastScrollY = lastFocusY
    }

    /** Two fingers: the first clear movement decides between scrolling (parallel drag) and zooming (pinch). */
    private fun onTwoFingerMove(event: MotionEvent) {
        val span = span(event)
        val focusX = (event.getX(0) + event.getX(1)) * 0.5f
        val focusY = (event.getY(0) + event.getY(1)) * 0.5f
        if (twoFingerMode == TwoFinger.UNDECIDED) {
            twoFingerMode = when {
                abs(span - twoFingerStartSpan) > ZOOM_SLOP_PX -> TwoFinger.ZOOM
                abs(focusY - twoFingerStartY) > SCROLL_THRESHOLD_PX -> TwoFinger.SCROLL
                else -> TwoFinger.UNDECIDED
            }
        }
        when (twoFingerMode) {
            TwoFinger.ZOOM -> {
                if (lastSpan > 0f) viewport.zoomBy(span / lastSpan, focusX, focusY)
                viewport.panBy(focusX - lastFocusX, focusY - lastFocusY)
                applyMatrix()
            }
            TwoFinger.SCROLL -> {
                val dy = focusY - lastScrollY
                if (abs(dy) > SCROLL_THRESHOLD_PX) {
                    sendWheel(if (dy > 0) SCROLL_STEP else -SCROLL_STEP)
                    lastScrollY = focusY
                }
            }
            TwoFinger.UNDECIDED -> Unit
        }
        lastSpan = span
        lastFocusX = focusX
        lastFocusY = focusY
    }

    private fun span(event: MotionEvent): Float = hypot(event.getX(0) - event.getX(1), event.getY(0) - event.getY(1))

    private fun onMove(event: MotionEvent) {
        if (scrollMode) {
            val dy = event.y - lastTouchY
            if (abs(dy) > SCROLL_THRESHOLD_PX) {
                sendWheel(if (dy > 0) SCROLL_STEP else -SCROLL_STEP)
                lastTouchY = event.y
            }
            return
        }
        if (trackpadMode) {
            // Relative to the drawn frame: the same on-screen speed at any zoom, so zooming in gives finer control.
            cursorX = (cursorX + (event.x - lastTouchX) / viewport.drawnWidth.coerceAtLeast(1f) * TRACKPAD_SENSITIVITY).coerceIn(0f, 1f)
            cursorY = (cursorY + (event.y - lastTouchY) / viewport.drawnHeight.coerceAtLeast(1f) * TRACKPAD_SENSITIVITY).coerceIn(0f, 1f)
            lastTouchX = event.x
            lastTouchY = event.y
            if (viewport.zoom > 1f) {
                viewport.ensureVisible(cursorX, cursorY, CURSOR_MARGIN_PX)
                applyMatrix()
            } else {
                updateCursorView()
            }
        } else {
            pointAt(event.x, event.y)
        }
        sendMove(force = false)
    }

    private fun pointAt(touchX: Float, touchY: Float) {
        cursorX = viewport.toNormX(touchX)
        cursorY = viewport.toNormY(touchY)
        updateCursorView()
    }

    // ---- Drawing ----

    /** View or frame size changed: recompute the fit and keep zoom and pan within bounds. */
    private fun layoutFrame() {
        val frame = ivScreen.drawable
        viewport.resize(ivScreen.width, ivScreen.height, frame?.intrinsicWidth ?: 0, frame?.intrinsicHeight ?: 0)
        applyMatrix()
    }

    private fun applyMatrix() {
        imageMatrix.setScale(viewport.scale, viewport.scale)
        imageMatrix.postTranslate(viewport.left, viewport.top)
        ivScreen.imageMatrix = imageMatrix
        updateCursorView()
    }

    private fun updateCursorView() {
        // The cursor and the frame share the container's padded origin.
        virtualCursor.translationX = viewport.toViewX(cursorX) - virtualCursor.width * 0.5f
        virtualCursor.translationY = viewport.toViewY(cursorY) - virtualCursor.height * 0.5f
    }

    /** Main thread: shows a decoded frame. */
    private fun showFrame(bitmap: Bitmap, fps: Int) {
        if (isDestroyed) return
        val sizeChanged = bitmap.width != viewport.imageWidth || bitmap.height != viewport.imageHeight
        ivScreen.setImageBitmap(bitmap)
        retainedFrame = bitmap
        if (sizeChanged) layoutFrame()
        if (connectingOverlay.visibility != View.GONE) connectingOverlay.visibility = View.GONE
        if (fps > 0) lastFps = fps
        updateStatusBadge()
    }

    private var lastFps = 0

    private fun updateStatusBadge() {
        val view = overlay ?: return
        view.findViewById<TextView>(R.id.tv_partner).text = partnerName
        view.findViewById<TextView>(R.id.tv_remote_fps).text = getString(R.string.fps_label, lastFps)
        view.findViewById<View>(R.id.tv_e2e).visibility = if (RelayConnection.isEndToEnd) View.VISIBLE else View.GONE
    }

    // ---- Input ----

    private fun sendMove(force: Boolean) {
        val now = SystemClock.uptimeMillis()
        if (!force && now - lastMoveSentMs < MOVE_THROTTLE_MS) return
        lastMoveSentMs = now
        RelayConnection.sendInput(InputEvent(action = "mousemove", normX = cursorX, normY = cursorY))
    }

    private fun sendMouse(action: String, button: Int) {
        RelayConnection.sendInput(InputEvent(action = action, normX = cursorX, normY = cursorY, button = button))
    }

    private fun sendWheel(deltaY: Int) {
        RelayConnection.sendInput(InputEvent(action = "wheel", normX = cursorX, normY = cursorY, deltaY = deltaY))
    }

    /**
     * Applies video messages on its own thread, in order: framed deltas build on the frame before them, so none may
     * be skipped. The socket thread queues them (at most [MAX_QUEUED]; beyond that the queue is dropped and a key
     * frame requested). At most one bitmap at a time waits for the main thread, always the newest.
     */
    private inner class FrameDecoder(private val render: (Bitmap, Int) -> Unit) {
        private val lock = Object()
        private val queue = ArrayDeque<ByteArray>()
        private var running = false

        /** Bumped by every start, so a thread left over from a quick stop/start exits instead of doubling up. */
        private var generation = 0
        private var lastKeyRequestMs = 0L

        // Newest frame waiting for the main thread; replaced, never queued.
        private val decoded = java.util.concurrent.atomic.AtomicReference<Pair<Bitmap, Int>?>(null)

        /** Socket thread. */
        fun offer(bytes: ByteArray) {
            val overflow = synchronized(lock) {
                val full = queue.size >= MAX_QUEUED
                if (full) queue.clear()
                queue.addLast(bytes)
                lock.notifyAll()
                full
            }
            if (overflow) requestKeyFrame()
        }

        fun start() {
            val gen = synchronized(lock) {
                if (running) return
                running = true
                queue.clear()
                ++generation
            }
            Thread({ loop(gen) }, "OpenDesk-FrameDecoder").apply {
                priority = Thread.MAX_PRIORITY
                start()
            }
            // A new screen has no base frame for the deltas that may follow.
            requestKeyFrame()
        }

        fun stop() {
            synchronized(lock) {
                running = false
                queue.clear()
                lock.notifyAll()
            }
        }

        /** Asks the host for a key frame, at most every [KEY_REQUEST_INTERVAL_MS]. */
        private fun requestKeyFrame() {
            val now = SystemClock.uptimeMillis()
            if (now - lastKeyRequestMs < KEY_REQUEST_INTERVAL_MS) return
            lastKeyRequestMs = now
            runOnUiThread { RelayConnection.sendInput(InputEvent(action = "video-keyframe")) }
        }

        private fun loop(gen: Int) {
            val compositor = FrameCompositor()
            var frames = 0
            var windowStart = SystemClock.uptimeMillis()
            var fps = 0
            while (true) {
                val (bytes, more) = synchronized(lock) {
                    while (running && generation == gen && queue.isEmpty()) {
                        try {
                            lock.wait()
                        } catch (_: InterruptedException) {
                            return
                        }
                    }
                    if (!running || generation != gen) return
                    queue.removeFirst() to queue.isNotEmpty()
                }
                when (compositor.apply(bytes)) {
                    FrameCompositor.Result.NEED_KEY -> {
                        requestKeyFrame()
                        continue
                    }
                    FrameCompositor.Result.UNCHANGED -> continue
                    FrameCompositor.Result.UPDATED -> {}
                }
                frames++
                val now = SystemClock.uptimeMillis()
                if (now - windowStart >= 1000L) {
                    fps = ((frames * 1000f) / (now - windowStart)).toInt().coerceAtLeast(1)
                    frames = 0
                    windowStart = now
                }
                // Copy only what the screen can show: skip while newer messages wait, or a render is pending.
                if (more || decoded.get() != null) continue
                val bitmap = compositor.snapshot() ?: continue
                if (decoded.getAndSet(bitmap to fps) == null) {
                    runOnUiThread { decoded.getAndSet(null)?.let { (b, f) -> render(b, f) } }
                }
            }
        }
    }
}
