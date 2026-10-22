package com.opendesk.app

import android.Manifest
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.RectF
import android.os.Bundle
import android.os.SystemClock
import android.util.Base64
import android.view.GestureDetector
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.widget.Button
import android.widget.ImageView
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.google.gson.JsonObject
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.math.abs

class RemoteViewerActivity : AppCompatActivity() {

    companion object {
        var gatewayClient: GatewayClient? = null
        var targetDeskClean: String = ""
        @Volatile
        var instance: RemoteViewerActivity? = null
    }

    private lateinit var ivScreen: ImageView
    private lateinit var viewVirtualCursor: View
    private lateinit var layoutConnectingOverlay: View
    private lateinit var tvRemoteFps: TextView
    private lateinit var btnExit: Button
    private lateinit var btnToggleMic: Button
    private lateinit var btnSwitchMonitor: Button
    private lateinit var btnToggleMode: Button
    private lateinit var btnLeftClick: Button
    private lateinit var btnDoubleClick: Button
    private lateinit var btnRightClick: Button
    private lateinit var btnHoldDrag: Button
    private lateinit var btnScrollUp: Button
    private lateinit var btnScrollDown: Button

    // Latest-frame-wins lock-free slots (prevents UI & decoder queue buildup)
    private val latestBinaryFrame = AtomicReference<ByteArray?>(null)
    private val latestBase64Frame = AtomicReference<String?>(null)
    private val isDecoderRunning = AtomicBoolean(false)
    private val uiRenderPending = AtomicBoolean(false)
    private var decoderThread: Thread? = null

    // Virtual cursor state in normalized coordinates [0.0 .. 1.0]
    private var cursorNormX = 0.5f
    private var cursorNormY = 0.5f
    private var isTrackpadMode = true
    private var isLeftHoldActive = false

    // Touch tracking state
    private var lastTouchX = 0f
    private var lastTouchY = 0f
    private var lastMoveSentMs = 0L
    private var twoFingerDownTime = 0L
    private var twoFingerMoved = false
    private var lastTwoFingerY = 0f

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        instance = this
        setContentView(R.layout.activity_remote)

        ivScreen = findViewById(R.id.iv_remote_screen)
        viewVirtualCursor = findViewById(R.id.view_virtual_cursor)
        layoutConnectingOverlay = findViewById(R.id.layout_connecting_overlay)
        tvRemoteFps = findViewById(R.id.tv_remote_fps)
        btnExit = findViewById(R.id.btn_exit_remote)
        btnToggleMic = findViewById(R.id.btn_toggle_mic)
        btnSwitchMonitor = findViewById(R.id.btn_switch_monitor)
        btnToggleMode = findViewById(R.id.btn_toggle_mode)
        btnLeftClick = findViewById(R.id.btn_left_click)
        btnDoubleClick = findViewById(R.id.btn_double_click)
        btnRightClick = findViewById(R.id.btn_right_click)
        btnHoldDrag = findViewById(R.id.btn_hold_drag)
        btnScrollUp = findViewById(R.id.btn_scroll_up)
        btnScrollDown = findViewById(R.id.btn_scroll_down)

        viewVirtualCursor.visibility = View.VISIBLE
        ivScreen.post { updateVirtualCursorPosition() }

        VoiceCallManager.startSession(gatewayClient, targetDeskClean)
        updateMicButtonUI()

        setupControlButtons()
        setupTouchAndGestures()
        startFrameDecoderWorker()
    }

    private fun setupControlButtons() {
        btnExit.setOnClickListener {
            disconnectAndFinish()
        }

        btnToggleMic.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            if (VoiceCallManager.isMuted) {
                if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                    ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.RECORD_AUDIO), 205)
                } else {
                    VoiceCallManager.setMicMuted(false, gatewayClient)
                    updateMicButtonUI()
                }
            } else {
                VoiceCallManager.setMicMuted(true, gatewayClient)
                updateMicButtonUI()
            }
        }

        btnToggleMode.setOnClickListener {
            isTrackpadMode = !isTrackpadMode
            btnToggleMode.text = if (isTrackpadMode) "Trackpad" else "Sentuh"
            updateStatusBadge(lastFpsDisplay, lastFrameW, lastFrameH)
        }

        btnSwitchMonitor.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            val cmd = JsonObject().apply {
                addProperty("action", "switch-monitor")
            }
            sendInputEvent(cmd)
            Toast.makeText(this, "Mengganti layar monitor...", Toast.LENGTH_SHORT).show()
        }

        btnLeftClick.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            sendMouseAction("click", button = 0)
        }

        btnDoubleClick.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
            sendMouseAction("dblclick", button = 0)
        }

        btnRightClick.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            sendMouseAction("click", button = 2)
        }

        btnHoldDrag.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            toggleHoldDrag(!isLeftHoldActive)
        }

        btnScrollUp.setOnClickListener {
            sendWheelScroll(120)
        }

        btnScrollDown.setOnClickListener {
            sendWheelScroll(-120)
        }
    }

    private fun updateMicButtonUI() {
        if (VoiceCallManager.isMuted) {
            btnToggleMic.text = "Mic: Mati"
            btnToggleMic.setBackgroundColor(0x00000000)
        } else {
            btnToggleMic.text = "Mic: Aktif"
            btnToggleMic.setBackgroundColor(0xFFDC2626.toInt())
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == 205 && grantResults.isNotEmpty() && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            VoiceCallManager.setMicMuted(false, gatewayClient)
            updateMicButtonUI()
        }
    }

    private fun toggleHoldDrag(active: Boolean) {
        isLeftHoldActive = active
        if (isLeftHoldActive) {
            btnHoldDrag.text = "Tahan (Aktif)"
            btnHoldDrag.setBackgroundColor(0xFFDC2626.toInt())
            sendMouseAction("mousedown", button = 0)
        } else {
            btnHoldDrag.text = "Tahan"
            btnHoldDrag.setBackgroundColor(0x00000000)
            sendMouseAction("mouseup", button = 0)
        }
    }

    private fun setupTouchAndGestures() {
        val gestureDetector = GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
            override fun onSingleTapConfirmed(e: MotionEvent): Boolean {
                if (!isTrackpadMode) {
                    updateCursorFromDirectTouch(e.x, e.y)
                }
                if (isLeftHoldActive) {
                    toggleHoldDrag(false)
                } else {
                    sendMouseAction("click", button = 0)
                }
                return true
            }

            override fun onDoubleTap(e: MotionEvent): Boolean {
                if (!isTrackpadMode) {
                    updateCursorFromDirectTouch(e.x, e.y)
                }
                ivScreen.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                sendMouseAction("dblclick", button = 0)
                return true
            }

            override fun onLongPress(e: MotionEvent) {
                if (!isTrackpadMode) {
                    updateCursorFromDirectTouch(e.x, e.y)
                }
                ivScreen.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                // Long press triggers Right-Click (AnyDesk mobile standard)
                sendMouseAction("click", button = 2)
                Toast.makeText(this@RemoteViewerActivity, "Klik Kanan", Toast.LENGTH_SHORT).show()
            }
        })

        ivScreen.setOnTouchListener { _, event ->
            gestureDetector.onTouchEvent(event)

            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    lastTouchX = event.x
                    lastTouchY = event.y
                    if (!isTrackpadMode) {
                        updateCursorFromDirectTouch(event.x, event.y)
                        sendMouseMoveThrottled(force = true)
                    }
                }

                MotionEvent.ACTION_POINTER_DOWN -> {
                    if (event.pointerCount == 2) {
                        twoFingerDownTime = SystemClock.uptimeMillis()
                        twoFingerMoved = false
                        lastTwoFingerY = (event.getY(0) + event.getY(1)) * 0.5f
                    }
                }

                MotionEvent.ACTION_MOVE -> {
                    if (event.pointerCount >= 2) {
                        // Two-finger vertical drag -> Mouse Wheel Scroll
                        val avgY = (event.getY(0) + event.getY(1)) * 0.5f
                        val dy = avgY - lastTwoFingerY
                        if (abs(dy) > 18f) {
                            twoFingerMoved = true
                            val wheelDelta = if (dy > 0) 120 else -120
                            sendWheelScroll(wheelDelta)
                            lastTwoFingerY = avgY
                        }
                    } else if (event.pointerCount == 1) {
                        if (isTrackpadMode) {
                            val w = ivScreen.width.coerceAtLeast(1).toFloat()
                            val h = ivScreen.height.coerceAtLeast(1).toFloat()
                            val dx = (event.x - lastTouchX) / w
                            val dy = (event.y - lastTouchY) / h
                            lastTouchX = event.x
                            lastTouchY = event.y

                            // Smooth 1.35x sensitivity for comfortable one-thumb desktop traversal
                            cursorNormX = (cursorNormX + dx * 1.35f).coerceIn(0f, 1f)
                            cursorNormY = (cursorNormY + dy * 1.35f).coerceIn(0f, 1f)
                            updateVirtualCursorPosition()
                            sendMouseMoveThrottled(force = false)
                        } else {
                            updateCursorFromDirectTouch(event.x, event.y)
                            sendMouseMoveThrottled(force = false)
                        }
                    }
                }

                MotionEvent.ACTION_POINTER_UP -> {
                    // Two-finger quick tap -> Instant Right Click
                    if (event.pointerCount == 2 && !twoFingerMoved &&
                        SystemClock.uptimeMillis() - twoFingerDownTime < 300L
                    ) {
                        ivScreen.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                        sendMouseAction("click", button = 2)
                    }
                }

                MotionEvent.ACTION_UP -> {
                    sendMouseMoveThrottled(force = true)
                }
            }
            true
        }
    }

    private fun getDisplayedImageRect(): RectF {
        val drawable = ivScreen.drawable
        if (drawable == null || drawable.intrinsicWidth <= 0 || drawable.intrinsicHeight <= 0) {
            return RectF(0f, 0f, ivScreen.width.toFloat().coerceAtLeast(1f), ivScreen.height.toFloat().coerceAtLeast(1f))
        }
        val rect = RectF(0f, 0f, drawable.intrinsicWidth.toFloat(), drawable.intrinsicHeight.toFloat())
        ivScreen.imageMatrix.mapRect(rect)
        return rect
    }

    private fun updateCursorFromDirectTouch(touchX: Float, touchY: Float) {
        val rect = getDisplayedImageRect()
        val w = rect.width().coerceAtLeast(1f)
        val h = rect.height().coerceAtLeast(1f)
        cursorNormX = ((touchX - rect.left) / w).coerceIn(0f, 1f)
        cursorNormY = ((touchY - rect.top) / h).coerceIn(0f, 1f)
        updateVirtualCursorPosition()
    }

    private fun updateVirtualCursorPosition() {
        val rect = getDisplayedImageRect()
        val px = rect.left + cursorNormX * rect.width()
        val py = rect.top + cursorNormY * rect.height()
        viewVirtualCursor.translationX = px - (viewVirtualCursor.width * 0.5f)
        viewVirtualCursor.translationY = py - (viewVirtualCursor.height * 0.5f)
    }

    private fun sendMouseMoveThrottled(force: Boolean) {
        val now = SystemClock.uptimeMillis()
        if (!force && now - lastMoveSentMs < 15L) {
            return
        }
        lastMoveSentMs = now
        val moveCmd = JsonObject().apply {
            addProperty("action", "mousemove")
            addProperty("normX", cursorNormX)
            addProperty("normY", cursorNormY)
        }
        sendInputEvent(moveCmd)
    }

    private fun sendMouseAction(action: String, button: Int) {
        val cmd = JsonObject().apply {
            addProperty("action", action)
            addProperty("button", button)
            addProperty("normX", cursorNormX)
            addProperty("normY", cursorNormY)
        }
        sendInputEvent(cmd)
    }

    private fun sendWheelScroll(deltaY: Int) {
        val cmd = JsonObject().apply {
            addProperty("action", "wheel")
            addProperty("deltaY", deltaY)
            addProperty("normX", cursorNormX)
            addProperty("normY", cursorNormY)
        }
        sendInputEvent(cmd)
    }

    private fun sendInputEvent(eventObj: JsonObject) {
        if (targetDeskClean.isEmpty()) return
        val relay = JsonObject().apply {
            addProperty("type", "relay-input")
            addProperty("targetClean", targetDeskClean)
            add("event", eventObj)
        }
        gatewayClient?.send(relay)
    }

    // Called from OkHttp reader thread: lock-free latest-frame-wins
    fun enqueueBinaryFrame(jpegBytes: ByteArray) {
        latestBinaryFrame.set(jpegBytes)
    }

    fun enqueueBase64Frame(base64Data: String) {
        latestBase64Frame.set(base64Data)
    }

    private var lastFpsDisplay = 60
    private var lastFrameW = 0
    private var lastFrameH = 0

    private fun updateStatusBadge(fps: Int, w: Int, h: Int) {
        val modeLabel = if (isTrackpadMode) "Trackpad" else "Sentuh"
        val resLabel = if (w > 0 && h > 0) "${w}x${h} • " else ""
        tvRemoteFps.text = "${resLabel}${fps} FPS • $modeLabel"
    }

    private fun startFrameDecoderWorker() {
        isDecoderRunning.set(true)
        decoderThread = Thread({
            val decodeOpts = BitmapFactory.Options().apply {
                inPreferredConfig = Bitmap.Config.RGB_565
                inMutable = true
            }
            var framesDecoded = 0
            var fpsWindowStart = SystemClock.uptimeMillis()
            var firstFrameRendered = false

            while (isDecoderRunning.get()) {
                val binBytes = latestBinaryFrame.getAndSet(null)
                val b64Str = if (binBytes == null) latestBase64Frame.getAndSet(null) else null

                if (binBytes == null && b64Str == null) {
                    try {
                        Thread.sleep(2)
                    } catch (_: InterruptedException) {
                        break
                    }
                    continue
                }

                try {
                    val rawBytes = binBytes ?: Base64.decode(b64Str, Base64.DEFAULT)
                    val bmp = BitmapFactory.decodeByteArray(rawBytes, 0, rawBytes.size, decodeOpts)
                    if (bmp != null) {
                        framesDecoded++
                        val now = SystemClock.uptimeMillis()
                        val elapsed = now - fpsWindowStart
                        if (elapsed >= 1000L) {
                            lastFpsDisplay = ((framesDecoded * 1000f) / elapsed).toInt().coerceAtLeast(1)
                            framesDecoded = 0
                            fpsWindowStart = now
                        }
                        lastFrameW = bmp.width
                        lastFrameH = bmp.height

                        // Coalesce UI posts: never queue more than 1 frame runnable on the main thread
                        if (uiRenderPending.compareAndSet(false, true)) {
                            val fpsNow = lastFpsDisplay.coerceAtLeast(60)
                            val wNow = lastFrameW
                            val hNow = lastFrameH
                            runOnUiThread {
                                uiRenderPending.set(false)
                                ivScreen.setImageBitmap(bmp)
                                if (!firstFrameRendered) {
                                    firstFrameRendered = true
                                    layoutConnectingOverlay.visibility = View.GONE
                                    updateVirtualCursorPosition()
                                }
                                updateStatusBadge(fpsNow, wNow, hNow)
                            }
                        }
                    }
                } catch (_: Exception) {
                }
            }
        }, "OpenDesk-FrameDecoder").apply {
            priority = Thread.MAX_PRIORITY
            start()
        }
    }

    private fun disconnectAndFinish() {
        VoiceCallManager.stopSession()
        if (isLeftHoldActive) {
            sendMouseAction("mouseup", button = 0)
            isLeftHoldActive = false
        }
        if (targetDeskClean.isNotEmpty()) {
            val disc = JsonObject().apply {
                addProperty("type", "session-disconnect")
                addProperty("targetClean", targetDeskClean)
            }
            gatewayClient?.send(disc)
        }
        finish()
    }

    override fun onBackPressed() {
        disconnectAndFinish()
    }

    override fun onDestroy() {
        isDecoderRunning.set(false)
        decoderThread?.interrupt()
        VoiceCallManager.stopSession()
        if (instance == this) instance = null
        super.onDestroy()
    }
}
