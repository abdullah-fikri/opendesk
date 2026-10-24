package com.opendesk.app.ui.viewer

import android.Manifest
import android.annotation.SuppressLint
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.os.Bundle
import android.os.SystemClock
import android.view.GestureDetector
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.widget.Button
import android.widget.ImageView
import android.widget.TextView
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
import com.opendesk.app.data.dto.InputEvent
import com.opendesk.app.domain.Viewport
import com.opendesk.app.domain.WindowsKeys
import com.opendesk.app.platform.audio.VoiceCallManager
import com.opendesk.app.session.RelayConnection
import kotlin.math.abs
import kotlin.math.hypot

/**
 * Full-screen view of a computer this phone controls.
 *
 * Touch: tap = left click, double tap = double click, long press or two-finger tap = right click, two-finger
 * vertical drag = scroll wheel, pinch = zoom (moving the fingers while pinching pans). In trackpad mode one finger
 * moves a virtual cursor and a zoomed view follows it; in touch mode the cursor jumps to the finger.
 *
 * Frames are decoded on a worker thread, newest frame only, so a slow phone never builds up latency. The
 * activity handles rotation itself (see the manifest), so the session, zoom and last frame survive it.
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
    }

    /** What the current two-finger gesture turned out to be. */
    private enum class TwoFinger { UNDECIDED, SCROLL, ZOOM }

    private lateinit var root: View
    private lateinit var ivScreen: ImageView
    private lateinit var virtualCursor: View
    private lateinit var connectingOverlay: View
    private lateinit var tvRemoteFps: TextView
    private lateinit var btnToggleMic: Button
    private lateinit var btnToggleMode: Button
    private lateinit var btnHoldDrag: Button
    private lateinit var keyboard: RemoteKeyboardView

    private val viewport = Viewport()
    private val imageMatrix = Matrix()
    private val decoder = FrameDecoder(::showFrame)

    // Virtual cursor in normalized coordinates [0..1].
    private var cursorX = 0.5f
    private var cursorY = 0.5f
    private var trackpadMode = true
    private var leftHeld = false
    private var keyboardVisible = false

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
        tvRemoteFps = findViewById(R.id.tv_remote_fps)
        btnToggleMic = findViewById(R.id.btn_toggle_mic)
        btnToggleMode = findViewById(R.id.btn_toggle_mode)
        btnHoldDrag = findViewById(R.id.btn_hold_drag)
        keyboard = findViewById(R.id.view_remote_keyboard)

        setupWindow()
        virtualCursor.visibility = View.VISIBLE
        ivScreen.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> layoutFrame() }
        retainedFrame?.let { showFrame(it, 0) }
        updateMicButton()
        updateStatusBadge()
        setupButtons()
        setupKeyboard()
        setupTouch()
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() = finish()
        })
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
        RelayConnection.endViewing()
        retainedFrame = null
        isOpen = false
    }

    // ---- RelayConnection.ViewerListener ----

    override fun onViewerSessionEnded(message: String?) {
        finish()
    }

    override fun onMicMuted() = updateMicButton()

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
            keyboardVisible = insets.isVisible(WindowInsetsCompat.Type.ime())
            WindowInsetsCompat.CONSUMED
        }
    }

    // ---- Buttons ----

    private fun setupButtons() {
        findViewById<Button>(R.id.btn_exit_remote).setOnClickListener { finish() }
        btnToggleMic.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            toggleMic()
        }
        btnToggleMode.setOnClickListener {
            trackpadMode = !trackpadMode
            btnToggleMode.setText(if (trackpadMode) R.string.mode_trackpad else R.string.mode_touch)
            updateStatusBadge()
        }
        findViewById<Button>(R.id.btn_switch_monitor).setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            RelayConnection.sendInput(InputEvent(action = "switch-monitor"))
            Toast.makeText(this, R.string.switching_monitor, Toast.LENGTH_SHORT).show()
        }
        findViewById<Button>(R.id.btn_left_click).setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            sendMouse("click", BUTTON_LEFT)
        }
        findViewById<Button>(R.id.btn_double_click).setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            sendMouse("dblclick", BUTTON_LEFT)
        }
        findViewById<Button>(R.id.btn_right_click).setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            sendMouse("click", BUTTON_RIGHT)
        }
        btnHoldDrag.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            setLeftHeld(!leftHeld)
        }
        findViewById<Button>(R.id.btn_scroll_up).setOnClickListener { sendWheel(SCROLL_STEP) }
        findViewById<Button>(R.id.btn_scroll_down).setOnClickListener { sendWheel(-SCROLL_STEP) }
        findViewById<Button>(R.id.btn_keyboard).setOnClickListener { keyboard.setKeyboardVisible(!keyboardVisible) }
        findViewById<Button>(R.id.btn_key_esc).setOnClickListener { sendKey(WindowsKeys.VK_ESCAPE) }
        findViewById<Button>(R.id.btn_key_tab).setOnClickListener { sendKey(WindowsKeys.VK_TAB) }
        findViewById<Button>(R.id.btn_key_win).setOnClickListener {
            RelayConnection.sendInput(InputEvent(action = "shortcut", name = "win"))
        }
        findViewById<Button>(R.id.btn_fit).setOnClickListener {
            viewport.reset()
            applyMatrix()
        }
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

    private fun updateMicButton() {
        val muted = VoiceCallManager.isMuted
        btnToggleMic.setText(if (muted) R.string.mic_off else R.string.mic_on)
        btnToggleMic.setBackgroundColor(ContextCompat.getColor(this, if (muted) android.R.color.transparent else R.color.accent))
    }

    private fun setLeftHeld(held: Boolean) {
        leftHeld = held
        btnHoldDrag.setText(if (held) R.string.hold_active else R.string.hold)
        btnHoldDrag.setBackgroundColor(ContextCompat.getColor(this, if (held) R.color.accent else android.R.color.transparent))
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
        val mode = getString(if (trackpadMode) R.string.mode_trackpad else R.string.mode_touch)
        val w = viewport.imageWidth
        val h = viewport.imageHeight
        val resolution = if (w > 0 && h > 0) "${w}x$h • " else ""
        tvRemoteFps.text = getString(R.string.viewer_status, resolution, lastFps, mode)
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
     * Decodes JPEG frames on its own thread. The socket thread only drops the newest frame into a slot; the decoder
     * sleeps until one arrives and hands at most one bitmap at a time to the main thread, always the newest.
     */
    private inner class FrameDecoder(private val render: (Bitmap, Int) -> Unit) {
        private val lock = Object()
        private var pending: ByteArray? = null
        private var running = false

        /** Bumped by every start, so a thread left over from a quick stop/start exits instead of doubling up. */
        private var generation = 0

        // Newest decoded bitmap waiting for the main thread; replaced, never queued.
        private val decoded = java.util.concurrent.atomic.AtomicReference<Pair<Bitmap, Int>?>(null)

        /** Socket thread. */
        fun offer(bytes: ByteArray) {
            synchronized(lock) {
                pending = bytes
                lock.notifyAll()
            }
        }

        fun start() {
            val gen = synchronized(lock) {
                if (running) return
                running = true
                pending = null
                ++generation
            }
            Thread({ loop(gen) }, "OpenDesk-FrameDecoder").apply {
                priority = Thread.MAX_PRIORITY
                start()
            }
        }

        fun stop() {
            synchronized(lock) {
                running = false
                pending = null
                lock.notifyAll()
            }
        }

        private fun loop(gen: Int) {
            val options = BitmapFactory.Options().apply { inPreferredConfig = Bitmap.Config.RGB_565 }
            var frames = 0
            var windowStart = SystemClock.uptimeMillis()
            var fps = 0
            while (true) {
                val bytes = synchronized(lock) {
                    while (running && generation == gen && pending == null) {
                        try {
                            lock.wait()
                        } catch (_: InterruptedException) {
                            return
                        }
                    }
                    if (!running || generation != gen) return
                    pending.also { pending = null }
                } ?: continue
                val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options) ?: continue
                frames++
                val now = SystemClock.uptimeMillis()
                if (now - windowStart >= 1000L) {
                    fps = ((frames * 1000f) / (now - windowStart)).toInt().coerceAtLeast(1)
                    frames = 0
                    windowStart = now
                }
                // Only schedule a render when none is waiting; a waiting one picks up this newer bitmap.
                if (decoded.getAndSet(bitmap to fps) == null) {
                    runOnUiThread { decoded.getAndSet(null)?.let { (b, f) -> render(b, f) } }
                }
            }
        }
    }
}
