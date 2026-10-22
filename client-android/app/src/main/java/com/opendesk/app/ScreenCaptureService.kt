package com.opendesk.app

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
import android.util.Base64
import android.util.DisplayMetrics
import android.view.WindowManager
import com.google.gson.JsonObject
import java.io.ByteArrayOutputStream

class ScreenCaptureService : Service() {

    companion object {
        var mediaProjection: MediaProjection? = null
        var targetClean: String? = null
        var gatewayClient: GatewayClient? = null
    }

    private var virtualDisplay: VirtualDisplay? = null
    private var imageReader: ImageReader? = null
    private var isCapturing = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        val notif = createNotification()
        startForeground(101, notif)
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

        val width = 1080
        val height = (1080 * metrics.heightPixels) / metrics.widthPixels.coerceAtLeast(1)
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
                        bmp.compress(Bitmap.CompressFormat.JPEG, 80, stream)
                        val jpegBytes = stream.toByteArray()
                        bmp.recycle()

                        val client = gatewayClient
                        if (client != null) {
                            client.sendBinary(jpegBytes)
                        }
                    }
                    val elapsedMs = (System.nanoTime() - startNs) / 1_000_000
                    if (elapsedMs < 16) {
                        Thread.sleep(16 - elapsedMs) // 60 FPS target
                    }
                } catch (e: Exception) {}
            }
        }.start()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                "opendesk_capture",
                "OpenDesk Screen Sharing",
                NotificationManager.IMPORTANCE_LOW
            )
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(channel)
        }
    }

    private fun createNotification(): Notification {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, "opendesk_capture")
                .setContentTitle("OpenDesk Aktif")
                .setContentText("Layar HP sedang dibagikan ke PC remote.")
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .build()
        } else {
            Notification.Builder(this)
                .setContentTitle("OpenDesk Aktif")
                .setContentText("Layar HP sedang dibagikan ke PC remote.")
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
