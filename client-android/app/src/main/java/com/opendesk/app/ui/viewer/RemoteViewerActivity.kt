package com.opendesk.app.ui.viewer

import android.Manifest
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.RectF
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
import com.opendesk.app.R
import com.opendesk.app.data.dto.InputEvent
import com.opendesk.app.platform.audio.VoiceCallManager
import com.opendesk.app.session.RelayConnection
import java.lang.ref.WeakReference
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.math.abs

/**
 * Full-screen view of a PC this phone controls. Touch drives a virtual cursor (trackpad mode) or points
 * directly (touch mode); buttons send clicks, holds and scrolls. Frames are decoded on a worker thread with
 * latest-frame-wins, so a slow phone never builds up latency.
 */
class RemoteViewerActivity : AppCompatActivity() {

    companion object {
        private var current = WeakReference<RemoteViewerActivity>(null)

        /** Closes the viewer when the session ends elsewhere. */
        fun finishIfOpen() {
            current.get()?.finish()
        }

        private const val MOVE_THROTTLE_MS = 15L
        private const val TRACKPAD_SENSITIVITY = 1.35f
        private const val SCROLL_STEP = 120
        private const val SCROLL_THRESHOLD_PX = 18f
        private const val TWO_FINGER_TAP_MS = 300L
        private const val BUTTON_LEFT = 0
        private const val BUTTON_RIGHT = 2
    }

    private lateinit var ivScreen: ImageView
    private lateinit var virtualCursor: View
    private lateinit var connectingOverlay: View
    private lateinit var tvRemoteFps: TextView
    private lateinit var btnToggleMic: Button
    private lateinit var btnToggleMode: Button
    private lateinit var btnHoldDrag: Button

    // Latest-frame-wins slot filled on the socket thread.
    private val latestFrame = AtomicReference<ByteArray?>(null)
    private val decoderRunning = AtomicBoolean(false)
    private val renderPending = AtomicBoolean(false)
    private var decoderThread: Thread? = null

    // Virtual cursor in normalized coordinates [0..1].
    private var cursorX = 0.5f
    private var cursorY = 0.5f
    private var trackpadMode = true
    private var leftHeld = false

    private var lastTouchX = 0f
    private var lastTouchY = 0f
    private var lastMoveSentMs = 0L
    private var twoFingerDownTime = 0L
    private var twoFingerMoved = false
    private var lastTwoFingerY = 0f

    private var lastFps = 0
    private var lastFrameW = 0
    private var lastFrameH = 0

    private val micPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            VoiceCallManager.setMicMuted(false)
            updateMicButton()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = WeakReference(this)
        setContentView(R.layout.activity_remote)

        ivScreen = findViewById(R.id.iv_remote_screen)
        virtualCursor = findViewById(R.id.view_virtual_cursor)
        connectingOverlay = findViewById(R.id.layout_connecting_overlay)
        tvRemoteFps = findViewById(R.id.tv_remote_fps)
        btnToggleMic = findViewById(R.id.btn_toggle_mic)
        btnToggleMode = findViewById(R.id.btn_toggle_mode)
        btnHoldDrag = findViewById(R.id.btn_hold_drag)

        virtualCursor.visibility = View.VISIBLE
        ivScreen.post { updateCursorView() }
        updateMicButton()
        setupButtons()
        setupTouch()
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() = disconnectAndFinish()
        })
        RelayConnection.videoSink = { latestFrame.set(it) }
        startDecoder()
    }

    override fun onDestroy() {
        RelayConnection.videoSink = null
        decoderRunning.set(false)
        decoderThread?.interrupt()
        if (current.get() === this) current = WeakReference(null)
        super.onDestroy()
    }

    private fun setupButtons() {
        findViewById<Button>(R.id.btn_exit_remote).setOnClickListener { disconnectAndFinish() }
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

    private fun setupTouch() {
        val gestures = GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
            override fun onSingleTapConfirmed(e: MotionEvent): Boolean {
                if (!trackpadMode) pointAt(e.x, e.y)
                if (leftHeld) setLeftHeld(false) else sendMouse("click", BUTTON_LEFT)
                return true
            }

            override fun onDoubleTap(e: MotionEvent): Boolean {
                if (!trackpadMode) pointAt(e.x, e.y)
                ivScreen.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                sendMouse("dblclick", BUTTON_LEFT)
                return true
            }

            override fun onLongPress(e: MotionEvent) {
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
                    lastTouchX = event.x
                    lastTouchY = event.y
                    if (!trackpadMode) {
                        pointAt(event.x, event.y)
                        sendMove(force = true)
                    }
                }
                MotionEvent.ACTION_POINTER_DOWN -> if (event.pointerCount == 2) {
                    twoFingerDownTime = SystemClock.uptimeMillis()
                    twoFingerMoved = false
                    lastTwoFingerY = (event.getY(0) + event.getY(1)) * 0.5f
                }
                MotionEvent.ACTION_MOVE -> onMove(event)
                // A quick two-finger tap is a right click.
                MotionEvent.ACTION_POINTER_UP -> if (event.pointerCount == 2 && !twoFingerMoved &&
                    SystemClock.uptimeMillis() - twoFingerDownTime < TWO_FINGER_TAP_MS
                ) {
                    ivScreen.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                    sendMouse("click", BUTTON_RIGHT)
                }
                MotionEvent.ACTION_UP -> sendMove(force = true)
            }
            true
        }
    }

    private fun onMove(event: MotionEvent) {
        if (event.pointerCount >= 2) {
            // Two-finger vertical drag scrolls.
            val avgY = (event.getY(0) + event.getY(1)) * 0.5f
            val dy = avgY - lastTwoFingerY
            if (abs(dy) > SCROLL_THRESHOLD_PX) {
                twoFingerMoved = true
                sendWheel(if (dy > 0) SCROLL_STEP else -SCROLL_STEP)
                lastTwoFingerY = avgY
            }
            return
        }
        if (trackpadMode) {
            val w = ivScreen.width.coerceAtLeast(1).toFloat()
            val h = ivScreen.height.coerceAtLeast(1).toFloat()
            cursorX = (cursorX + (event.x - lastTouchX) / w * TRACKPAD_SENSITIVITY).coerceIn(0f, 1f)
            cursorY = (cursorY + (event.y - lastTouchY) / h * TRACKPAD_SENSITIVITY).coerceIn(0f, 1f)
            lastTouchX = event.x
            lastTouchY = event.y
            updateCursorView()
        } else {
            pointAt(event.x, event.y)
        }
        sendMove(force = false)
    }

    /** Where the frame is drawn inside the image view (fit-center). */
    private fun displayedImageRect(): RectF {
        val drawable = ivScreen.drawable
        if (drawable == null || drawable.intrinsicWidth <= 0 || drawable.intrinsicHeight <= 0) {
            return RectF(0f, 0f, ivScreen.width.toFloat().coerceAtLeast(1f), ivScreen.height.toFloat().coerceAtLeast(1f))
        }
        val rect = RectF(0f, 0f, drawable.intrinsicWidth.toFloat(), drawable.intrinsicHeight.toFloat())
        ivScreen.imageMatrix.mapRect(rect)
        return rect
    }

    private fun pointAt(touchX: Float, touchY: Float) {
        val rect = displayedImageRect()
        cursorX = ((touchX - rect.left) / rect.width().coerceAtLeast(1f)).coerceIn(0f, 1f)
        cursorY = ((touchY - rect.top) / rect.height().coerceAtLeast(1f)).coerceIn(0f, 1f)
        updateCursorView()
    }

    private fun updateCursorView() {
        val rect = displayedImageRect()
        virtualCursor.translationX = rect.left + cursorX * rect.width() - virtualCursor.width * 0.5f
        virtualCursor.translationY = rect.top + cursorY * rect.height() - virtualCursor.height * 0.5f
    }

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

    private fun updateStatusBadge() {
        val mode = getString(if (trackpadMode) R.string.mode_trackpad else R.string.mode_touch)
        val resolution = if (lastFrameW > 0 && lastFrameH > 0) "${lastFrameW}x$lastFrameH • " else ""
        tvRemoteFps.text = getString(R.string.viewer_status, resolution, lastFps, mode)
    }

    private fun startDecoder() {
        decoderRunning.set(true)
        decoderThread = Thread({
            val options = BitmapFactory.Options().apply {
                inPreferredConfig = Bitmap.Config.RGB_565
                inMutable = true
            }
            var frames = 0
            var windowStart = SystemClock.uptimeMillis()
            var firstFrameShown = false
            while (decoderRunning.get()) {
                val bytes = latestFrame.getAndSet(null)
                if (bytes == null) {
                    try {
                        Thread.sleep(2)
                    } catch (_: InterruptedException) {
                        break
                    }
                    continue
                }
                val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options) ?: continue
                frames++
                val now = SystemClock.uptimeMillis()
                if (now - windowStart >= 1000L) {
                    lastFps = ((frames * 1000f) / (now - windowStart)).toInt().coerceAtLeast(1)
                    frames = 0
                    windowStart = now
                }
                lastFrameW = bitmap.width
                lastFrameH = bitmap.height
                // Never queue more than one frame on the main thread.
                if (renderPending.compareAndSet(false, true)) {
                    runOnUiThread {
                        renderPending.set(false)
                        ivScreen.setImageBitmap(bitmap)
                        if (!firstFrameShown) {
                            firstFrameShown = true
                            connectingOverlay.visibility = View.GONE
                            updateCursorView()
                        }
                        updateStatusBadge()
                    }
                }
            }
        }, "OpenDesk-FrameDecoder").apply {
            priority = Thread.MAX_PRIORITY
            start()
        }
    }

    private fun disconnectAndFinish() {
        if (leftHeld) {
            sendMouse("mouseup", BUTTON_LEFT)
            leftHeld = false
        }
        RelayConnection.endViewing()
        finish()
    }
}
