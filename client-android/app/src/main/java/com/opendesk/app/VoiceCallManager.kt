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

    fun startSession(client: GatewayClient?, partnerClean: String = "") {
        activeClient = client
        if (partnerClean.isNotEmpty()) targetClean = partnerClean
        isMicMuted.set(true)
        if (isSessionActive.compareAndSet(false, true)) {
            initAudioTrack()
        }
    }

    fun stopSession() {
        isSessionActive.set(false)
        isMicMuted.set(true)
        captureThread?.interrupt()
        captureThread = null
        try {
            audioTrack?.pause()
            audioTrack?.flush()
            audioTrack?.release()
        } catch (_: Exception) {}
        audioTrack = null
    }

    fun setMicMuted(muted: Boolean, client: GatewayClient?) {
        if (client != null) activeClient = client
        if (!isSessionActive.get()) {
            startSession(activeClient)
        }
        val prev = isMicMuted.getAndSet(muted)
        if (!muted && prev) {
            startMicCaptureLoop()
        }
    }

    private fun initAudioTrack() {
        try {
            val minBuf = AudioTrack.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_OUT_MONO,
                AudioFormat.ENCODING_PCM_16BIT
            ).coerceAtLeast(FRAME_BYTES * 4)

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
                        .setSampleRate(SAMPLE_RATE)
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
        if (!isSessionActive.get() || pcmBytes.isEmpty()) return
        val track = audioTrack ?: return
        try {
            track.write(pcmBytes, 0, pcmBytes.size)
        } catch (_: Exception) {}
    }

    @SuppressLint("MissingPermission")
    private fun startMicCaptureLoop() {
        captureThread?.interrupt()
        captureThread = Thread({
            var recorder: AudioRecord? = null
            var aec: AcousticEchoCanceler? = null
            var ns: NoiseSuppressor? = null
            try {
                val minBuf = AudioRecord.getMinBufferSize(
                    SAMPLE_RATE,
                    AudioFormat.CHANNEL_IN_MONO,
                    AudioFormat.ENCODING_PCM_16BIT
                ).coerceAtLeast(FRAME_BYTES * 4)

                recorder = AudioRecord(
                    MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                    SAMPLE_RATE,
                    AudioFormat.CHANNEL_IN_MONO,
                    AudioFormat.ENCODING_PCM_16BIT,
                    minBuf
                )

                if (recorder.state != AudioRecord.STATE_INITIALIZED) {
                    recorder.release()
                    return@Thread
                }

                val sessionId = recorder.audioSessionId
                if (AcousticEchoCanceler.isAvailable()) {
                    aec = AcousticEchoCanceler.create(sessionId)?.apply { enabled = true }
                }
                if (NoiseSuppressor.isAvailable()) {
                    ns = NoiseSuppressor.create(sessionId)?.apply { enabled = true }
                }

                recorder.startRecording()
                val pcmBuf = ByteArray(FRAME_BYTES)

                while (isSessionActive.get() && !isMicMuted.get()) {
                    val read = recorder.read(pcmBuf, 0, FRAME_BYTES)
                    if (read > 0) {
                        val b64 = Base64.encodeToString(pcmBuf, 0, read, Base64.NO_WRAP)
                        val tClean = targetClean
                        if (tClean.isNotEmpty()) {
                            val evt = JsonObject().apply {
                                addProperty("action", "audio")
                                addProperty("data", b64)
                            }
                            val msg = JsonObject().apply {
                                addProperty("type", "relay-input")
                                addProperty("targetClean", tClean)
                                add("event", evt)
                            }
                            activeClient?.send(msg)
                        } else {
                            val pkt = ByteArray(1 + read)
                            pkt[0] = 0x02
                            System.arraycopy(pcmBuf, 0, pkt, 1, read)
                            activeClient?.sendBinary(pkt)
                        }
                    }
                }
            } catch (_: Exception) {
            } finally {
                try {
                    aec?.release()
                    ns?.release()
                    recorder?.stop()
                    recorder?.release()
                } catch (_: Exception) {}
            }
        }, "OpenDesk-VoiceMic").apply {
            priority = Thread.MAX_PRIORITY
            start()
        }
    }
}
