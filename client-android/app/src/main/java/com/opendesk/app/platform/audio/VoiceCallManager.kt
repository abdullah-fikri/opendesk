package com.opendesk.app.platform.audio

import android.annotation.SuppressLint
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTrack
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Two-way voice for a session: plays the partner's packets and, while unmuted, captures the microphone
 * (16 kHz mono with echo cancellation and noise suppression) into `[0x02][16][pcm16le]` packets.
 */
object VoiceCallManager {

    private const val SAMPLE_RATE = 16000
    private const val FRAME_BYTES = 1280 // 40ms @ 16kHz 16-bit mono

    private val isSessionActive = AtomicBoolean(false)
    private val isMicMuted = AtomicBoolean(true)

    private var audioTrack: AudioTrack? = null
    private var captureThread: Thread? = null
    /** Where captured packets go; set per session. */
    @Volatile
    private var sink: ((ByteArray) -> Unit)? = null

    val isMuted: Boolean
        get() = isMicMuted.get()

    /** Starts playback for a session with `partnerClean`; captured packets go to `packetSink`. The microphone starts muted. */
    @Synchronized
    fun startSession(partnerClean: String, packetSink: (ByteArray) -> Unit) {
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
        sink = packetSink
        isMicMuted.set(true)
        if (isSessionActive.compareAndSet(false, true)) {
            initAudioTrack()
        }
    }

    /** Stops capture and playback and releases the audio devices. */
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

    /** Mutes or unmutes the microphone; unmuting starts the recorder and fails silently without the permission. */
    @Synchronized
    fun setMicMuted(muted: Boolean) {
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

    /** Plays a voice packet: `[0x02][rate_khz][pcm]` (even length) or the older `[0x02][pcm]` at 16 kHz. */
    fun playPacket(packet: ByteArray) {
        val (rateKhz, pcm) = if (packet.size % 2 == 0) {
            (packet[1].toInt() and 0xFF) to packet.copyOfRange(2, packet.size)
        } else {
            16 to packet.copyOfRange(1, packet.size)
        }
        playPcm(rateKhz, pcm)
    }

    private fun playPcm(rateKhz: Int, pcmBytes: ByteArray) {
        if (!isSessionActive.get() || pcmBytes.isEmpty()) return
        val targetRate = when (rateKhz) {
            24 -> 24000
            48 -> 48000
            else -> SAMPLE_RATE
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
                        sink?.invoke(pkt)
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
