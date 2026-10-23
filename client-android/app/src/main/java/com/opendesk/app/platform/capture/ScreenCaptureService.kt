package com.opendesk.app.platform.capture

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.IBinder
import android.util.DisplayMetrics
import android.view.WindowManager
import com.opendesk.app.R
import java.io.ByteArrayOutputStream

/**
 * Foreground service that mirrors the screen through MediaProjection and streams it as JPEG frames
 * (1080 px wide, quality 80, up to 60 FPS) while a PC controls this phone.
 */
class ScreenCaptureService : Service() {

    companion object {
        /** Granted by the user right before the service starts; consumed by the service. */
        var mediaProjection: MediaProjection? = null

        /** Receives every encoded frame. */
        var frameSink: ((ByteArray) -> Unit)? = null

        private const val CHANNEL_ID = "opendesk_capture"
        private const val NOTIFICATION_ID = 101
        private const val STREAM_WIDTH = 1080
        private const val JPEG_QUALITY = 80
        private const val FRAME_INTERVAL_MS = 16L
    }

    private var virtualDisplay: VirtualDisplay? = null
    private var imageReader: ImageReader? = null
    private var isCapturing = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        val notif = createNotification()
        startForeground(NOTIFICATION_ID, notif)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startScreenCapture()
        return START_STICKY
    }

    private fun startScreenCapture() {
        val proj = mediaProjection ?: return
        val wm = getSystemService(Context.WINDOW_SERVICE) as WindowManager
        val metrics = DisplayMetrics()
        wm.defaultDisplay.getRealMetrics(metrics)

        val width = STREAM_WIDTH
        val height = (STREAM_WIDTH * metrics.heightPixels) / metrics.widthPixels.coerceAtLeast(1)
        val dpi = metrics.densityDpi

        imageReader = ImageReader.newInstance(width, height, PixelFormat.RGBA_8888, 3)
        virtualDisplay = proj.createVirtualDisplay(
            "OpenDeskCapture",
            width, height, dpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            imageReader!!.surface, null, null
        )

        isCapturing = true
        Thread {
            while (isCapturing) {
                val startNs = System.nanoTime()
                try {
                    val img = imageReader?.acquireLatestImage()
                    if (img != null) {
                        val planes = img.planes
                        val buffer = planes[0].buffer
                        val pixelStride = planes[0].pixelStride
                        val rowStride = planes[0].rowStride
                        val rowPadding = rowStride - pixelStride * width

                        val fullBmp = Bitmap.createBitmap(
                            width + rowPadding / pixelStride,
                            height,
                            Bitmap.Config.ARGB_8888
                        )
                        fullBmp.copyPixelsFromBuffer(buffer)
                        img.close()

                        val bmp = if (rowPadding > 0) {
                            val cropped = Bitmap.createBitmap(fullBmp, 0, 0, width, height)
                            fullBmp.recycle()
                            cropped
                        } else {
                            fullBmp
                        }

                        // Compress to high-definition JPEG (Q80) and send via Binary WebSocket fast-path
                        val stream = ByteArrayOutputStream(65536)
                        bmp.compress(Bitmap.CompressFormat.JPEG, JPEG_QUALITY, stream)
                        val jpegBytes = stream.toByteArray()
                        bmp.recycle()

                        frameSink?.invoke(jpegBytes)
                    }
                    val elapsedMs = (System.nanoTime() - startNs) / 1_000_000
                    if (elapsedMs < FRAME_INTERVAL_MS) {
                        Thread.sleep(FRAME_INTERVAL_MS - elapsedMs)
                    }
                } catch (e: Exception) {}
            }
        }.start()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                getString(R.string.capture_channel_name),
                NotificationManager.IMPORTANCE_LOW
            )
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(channel)
        }
    }

    private fun createNotification(): Notification {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
                .setContentTitle(getString(R.string.capture_notification_title))
                .setContentText(getString(R.string.capture_notification_text))
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .build()
        } else {
            Notification.Builder(this)
                .setContentTitle(getString(R.string.capture_notification_title))
                .setContentText(getString(R.string.capture_notification_text))
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .build()
        }
    }

    override fun onDestroy() {
        isCapturing = false
        virtualDisplay?.release()
        imageReader?.close()
        super.onDestroy()
    }
}
