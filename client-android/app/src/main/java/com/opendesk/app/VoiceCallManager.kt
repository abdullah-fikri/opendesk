package com.opendesk.app

import android.annotation.SuppressLint
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTrack
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import android.util.Base64
import com.google.gson.JsonObject
import java.util.concurrent.atomic.AtomicBoolean

object VoiceCallManager {

    private const val SAMPLE_RATE = 16000
    private const val FRAME_BYTES = 1280 // 40ms @ 16kHz 16-bit mono

    private val isSessionActive = AtomicBoolean(false)
    private val isMicMuted = AtomicBoolean(true)

    private var audioTrack: AudioTrack? = null
    private var captureThread: Thread? = null
    private var activeClient: GatewayClient? = null
    @Volatile
    var targetClean: String = ""

    val isMuted: Boolean
        get() = isMicMuted.get()

    @Synchronized
    fun startSession(client: GatewayClient?, partnerClean: String = "") {
        if (captureThread?.isAlive == true && !isSessionActive.get()) {
            try {
                captureThread?.join(1000)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
                return
            }
            if (captureThread?.isAlive == true) return
            captureThread = null
        }
        activeClient = client
        if (partnerClean.isNotEmpty()) targetClean = partnerClean
        isMicMuted.set(true)
        if (isSessionActive.compareAndSet(false, true)) {
            initAudioTrack()
        }
    }

    @Synchronized
    fun stopSession() {
        isSessionActive.set(false)
        isMicMuted.set(true)
        captureThread?.interrupt()
        try {
            captureThread?.join(1000)
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        }
        if (captureThread?.isAlive != true) captureThread = null
        try {
            audioTrack?.pause()
            audioTrack?.flush()
            audioTrack?.release()
        } catch (_: Exception) {}
        audioTrack = null
    }

    @Synchronized
    fun setMicMuted(muted: Boolean, client: GatewayClient?) {
        if (client != null) activeClient = client
        if (!isSessionActive.get()) return
        if (!muted && captureThread?.isAlive == true && captureThread?.isInterrupted == true) return
        if (muted) {
            isMicMuted.set(true)
            return
        }
        // Report "active" only once a recorder is really running; it keeps running (discarding samples) while muted.
        if (captureThread?.isAlive == true || startMicCaptureLoop()) isMicMuted.set(false)
    }

    private var currentSampleRate = 16000

    fun playPcmPacket(rateKhz: Int, pcmBytes: ByteArray) {
        if (!isSessionActive.get() || pcmBytes.isEmpty()) return
        val targetRate = when (rateKhz) {
            16 -> 16000
            24 -> 24000
            48 -> 48000
            else -> 16000
        }
        if (audioTrack == null || currentSampleRate != targetRate) {
            reinitAudioTrack(targetRate)
        }
        val track = audioTrack ?: return
        try {
            track.write(pcmBytes, 0, pcmBytes.size)
        } catch (_: Exception) {}
    }

    private fun reinitAudioTrack(sampleRate: Int) {
        try {
            audioTrack?.pause()
            audioTrack?.flush()
            audioTrack?.release()
        } catch (_: Exception) {}
        currentSampleRate = sampleRate
        initAudioTrack(sampleRate)
    }

    private fun initAudioTrack(sampleRate: Int = SAMPLE_RATE) {
        try {
            val frameBytes = (sampleRate * 2 * 20) / 1000 // 20ms frame
            val minBuf = AudioTrack.getMinBufferSize(
                sampleRate,
                AudioFormat.CHANNEL_OUT_MONO,
                AudioFormat.ENCODING_PCM_16BIT
            ).coerceAtLeast(frameBytes * 4)

            val track = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build()
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(sampleRate)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                        .build()
                )
                .setBufferSizeInBytes(minBuf)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()

            track.play()
            audioTrack = track
        } catch (_: Exception) {}
    }

    fun playPcmChunk(pcmBytes: ByteArray) {
        playPcmPacket(16, pcmBytes)
    }

    /** Creates and starts the recorder on the caller's thread; returns false if the mic is unavailable. */
    @SuppressLint("MissingPermission")
    private fun startMicCaptureLoop(): Boolean {
        val recorder = try {
            val minBuf = AudioRecord.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT
            ).coerceAtLeast(FRAME_BYTES * 4)
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                minBuf
            )
        } catch (_: Exception) {
            return false
        }
        val started = recorder.state == AudioRecord.STATE_INITIALIZED &&
            runCatching { recorder.startRecording() }.isSuccess &&
            recorder.recordingState == AudioRecord.RECORDSTATE_RECORDING
        if (!started) {
            recorder.runCatching { release() }
            return false
        }
        captureThread = Thread({
            var aec: AcousticEchoCanceler? = null
            var ns: NoiseSuppressor? = null
            try {
                val sessionId = recorder.audioSessionId
                if (AcousticEchoCanceler.isAvailable()) {
                    aec = AcousticEchoCanceler.create(sessionId)?.apply { enabled = true }
                }
                if (NoiseSuppressor.isAvailable()) {
                    ns = NoiseSuppressor.create(sessionId)?.apply { enabled = true }
                }

                val pcmBuf = ByteArray(FRAME_BYTES)

                while (isSessionActive.get() && !Thread.currentThread().isInterrupted) {
                    val read = recorder.read(pcmBuf, 0, FRAME_BYTES)
                    if (read < 0) break // device lost (e.g. ERROR_DEAD_OBJECT)
                    if (read > 0 && !isMicMuted.get() && isSessionActive.get()) {
                        // Binary 0x02 fast-path: [0x02, rate_khz=16, ...pcm]
                        val pkt = ByteArray(2 + read)
                        pkt[0] = 0x02
                        pkt[1] = 16
                        System.arraycopy(pcmBuf, 0, pkt, 2, read)
                        activeClient?.sendBinary(pkt)
                    }
                }
            } catch (_: Exception) {
            } finally {
                // The recorder is gone, so the UI must stop claiming the mic is active.
                isMicMuted.set(true)
                try {
                    aec?.release()
                    ns?.release()
                    recorder.stop()
                    recorder.release()
                } catch (_: Exception) {}
            }
        }, "OpenDesk-VoiceMic").apply {
            priority = Thread.MAX_PRIORITY
            start()
        }
        return true
    }
}
