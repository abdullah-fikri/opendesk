package com.opendesk.app.session

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import com.opendesk.app.crypto.NoiseXX
import org.bouncycastle.util.encoders.Base64

/**
 * End-to-end encryption of one session, the counterpart of client-desktop/src/infra/e2e.rs (wire format in
 * client-desktop/docs/features/e2e.md). The viewer and the host run [NoiseXX] with their device keys over
 * `p2p-signal`; afterwards video, voice and input events on the relay path are sealed, keeping only the bytes the
 * relay routes on in clear:
 *
 * ```
 * video [FF D8 00 4F version kind flags 0xE2][nonce u64][segments]
 * voice [0x02 0xE2][nonce u64][segments]
 * input {"action":"e2e","d":base64(nonce u64, segments)}
 * ```
 *
 * The segments seal `[length u32][original message]` in pieces of at most one Noise message, with consecutive
 * nonces; the nonce's top byte is the stream, so streams never share a counter. A receiver accepts only increasing
 * nonces per stream and, once a stream delivered something sealed, drops plain messages on it.
 *
 * Thread-safe: frames are opened on the socket thread, voice is sealed on the audio thread, the rest runs on the
 * main thread.
 */
class E2eChannel private constructor(
    val partner: String,
    val isHost: Boolean,
    private var noise: NoiseXX?,
    transport: NoiseXX.Transport?,
) {
    /** One relay-path stream: its nonce domain and replay counter. */
    enum class Stream(val domain: Long) { VIDEO(1), AUDIO(2), FILE(3), INPUT(4) }

    private val lock = Any()

    @Volatile
    private var transport: NoiseXX.Transport? = transport
    private var failed = false
    private val tx = LongArray(STREAMS)
    private val rx = LongArray(STREAMS) { -1 }
    private val sealedSeen = BooleanArray(STREAMS)

    /** The partner's static key, once verified. */
    @Volatile
    var peerKey: ByteArray? = null
        private set

    /**
     * Outbound traffic is sealed: the host once it read the last handshake message, the viewer once the host
     * confirmed that (`e2e-ready`), so nothing sealed reaches a host that cannot open it yet.
     */
    @Volatile
    var isOn: Boolean = transport != null
        private set

    /** The handshake ended without keys (rejected key or a broken message). */
    val hasFailed: Boolean get() = synchronized(lock) { failed }

    /**
     * Handles one handshake signal. `verify` judges the peer's static key once it is known; false ends the
     * handshake (the viewer then never sends its last message, so neither side seals anything). Returns the
     * signal to send back, if any.
     */
    fun onSignal(signal: JsonObject, verify: (ByteArray) -> Boolean): String? = synchronized(lock) {
        if (signal.has("e2e-ready")) {
            if (!isHost && transport != null) isOn = true
            return null
        }
        val hs = noise ?: return null
        val msg = runCatching { Base64.decode(signal.get("e2e").asString) }.getOrNull()
        if (msg == null || msg.size > MAX_HANDSHAKE || !hs.readMessage(msg)) return fail()
        val key = hs.remoteStatic
        if (peerKey == null && key != null) {
            if (!verify(key)) return fail()
            peerKey = key
        }
        var reply: String? = null
        if (!hs.isFinished && hs.isMyTurn) reply = handshakeSignal(hs.writeMessage() ?: return fail())
        if (hs.isFinished) {
            transport = hs.transport
            noise = null
            if (isHost) {
                isOn = true
                reply = READY_SIGNAL
            }
        }
        reply
    }

    private fun fail(): String? {
        failed = true
        noise = null
        return null
    }

    /** Seals a video or voice message for the relay path; null sends it unchanged (not sealing yet, or plain JPEG). */
    fun seal(msg: ByteArray): ByteArray? {
        if (!isOn) return null
        val t = transport ?: return null
        val (stream, header) = classifyPlain(msg) ?: return null
        val nonce = reserve(stream, msg.size) ?: return null
        val out = java.io.ByteArrayOutputStream(header + 8 + 4 + msg.size + NoiseXX.TAG * (2 + msg.size / SEGMENT))
        val head = msg.copyOfRange(0, header)
        mark(stream, head)
        out.write(head)
        out.write(longBytes(nonce))
        sealSegments(t, nonce, msg, out)
        return out.toByteArray()
    }

    /**
     * Opens a binary message from the relay path. Returns the message itself when it is plain and allowed, the
     * original when it was sealed, and null to drop it (forged, replayed, or plain on a stream that is sealed now).
     */
    fun open(msg: ByteArray): ByteArray? = synchronized(lock) {
        val sealed = classifySealed(msg)
        if (sealed == null) {
            val stream = plainStream(msg)
            return if (stream != null && sealedSeen[stream.ordinal]) null else msg
        }
        val (stream, header) = sealed
        val t = transport ?: return null
        if (msg.size < header + 8) return null
        val nonce = readLong(msg, header)
        val (plain, last) = openChecked(t, stream, nonce, msg, header + 8) ?: return null
        if (plain.size < header) return null
        val expected = plain.copyOfRange(0, header)
        mark(stream, expected)
        if (!expected.contentEquals(msg.copyOfRange(0, header)) || classifyPlain(plain)?.first != stream) return null
        rx[stream.ordinal] = last
        sealedSeen[stream.ordinal] = true
        plain
    }

    /** Seals an input event (its JSON) for `relay-input`; null sends it unchanged. */
    fun sealEvent(eventJson: String): JsonObject? {
        if (!isOn) return null
        val t = transport ?: return null
        val json = eventJson.toByteArray(Charsets.UTF_8)
        val nonce = reserve(Stream.INPUT, json.size) ?: return null
        val out = java.io.ByteArrayOutputStream()
        out.write(longBytes(nonce))
        sealSegments(t, nonce, json, out)
        return JsonObject().apply {
            addProperty("action", "e2e")
            addProperty("d", Base64.toBase64String(out.toByteArray()))
        }
    }

    /** Opens an event from `input-command`; null drops it (forged, replayed, or plain once events are sealed). */
    fun openEvent(event: JsonObject): JsonObject? = synchronized(lock) {
        if (event.get("action")?.takeIf { it.isJsonPrimitive }?.asString != "e2e") {
            return if (sealedSeen[Stream.INPUT.ordinal]) null else event
        }
        val t = transport ?: return null
        val body = runCatching { Base64.decode(event.get("d").asString) }.getOrNull() ?: return null
        if (body.size < 8) return null
        val (plain, last) = openChecked(t, Stream.INPUT, readLong(body, 0), body, 8) ?: return null
        val value = runCatching { JsonParser.parseString(String(plain, Charsets.UTF_8)).asJsonObject }.getOrNull()
            ?: return null
        rx[Stream.INPUT.ordinal] = last
        sealedSeen[Stream.INPUT.ordinal] = true
        value
    }

    /** Takes the nonces for one message of `len` bytes on `stream`. */
    private fun reserve(stream: Stream, len: Int): Long? = synchronized(lock) {
        val counter = tx[stream.ordinal]
        val next = counter + segments(len)
        if (next >= NONCE_LIMIT) return null
        tx[stream.ordinal] = next
        (stream.domain shl DOMAIN_SHIFT) or counter
    }

    /** Opens the segments of `msg` from `offset`, after the domain and replay checks; caller holds the lock. */
    private fun openChecked(
        t: NoiseXX.Transport,
        stream: Stream,
        nonce: Long,
        msg: ByteArray,
        offset: Int,
    ): Pair<ByteArray, Long>? {
        if (nonce ushr DOMAIN_SHIFT != stream.domain || nonce <= rx[stream.ordinal]) return null
        val plain = java.io.ByteArrayOutputStream(msg.size - offset)
        var n = nonce
        var at = offset
        while (at < msg.size) {
            if (n ushr DOMAIN_SHIFT != stream.domain) return null
            val len = minOf(SEGMENT + NoiseXX.TAG, msg.size - at)
            plain.write(t.decrypt(n, msg, at, len) ?: return null)
            at += len
            n++
        }
        val bytes = plain.toByteArray()
        if (bytes.size < 4) return null
        val len = readInt(bytes, 0).toLong() and 0xFFFF_FFFFL
        if (bytes.size.toLong() != 4 + len) return null // a dropped or added segment
        return bytes.copyOfRange(4, bytes.size) to n - 1
    }

    companion object {
        private const val STREAMS = 4
        private const val SEALED: Byte = 0xE2.toByte()
        private const val FRAMED_HEADER = 8
        private const val AUDIO_MARKER: Byte = 0x02
        private const val SEGMENT = NoiseXX.MAX_PLAINTEXT
        private const val DOMAIN_SHIFT = 56
        private const val NONCE_LIMIT = 1L shl DOMAIN_SHIFT
        private const val MAX_HANDSHAKE = 256
        private val FRAMED_VIDEO = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0x00, 0x4F)
        private const val READY_SIGNAL = "{\"v\":2,\"e2e-ready\":true}"

        /**
         * Starts the handshake. The viewer initiates: the returned signal goes to the partner. `localClean` and
         * `partner` are clean desk IDs; they bind the handshake to this pair.
         */
        fun start(isHost: Boolean, localClean: String, partner: String, staticPrivate: ByteArray): Pair<E2eChannel, String?>? {
            val (viewer, host) = if (isHost) partner to localClean else localClean to partner
            val noise = NoiseXX(!isHost, staticPrivate, "opendesk-e2e-v1|$viewer|$host".toByteArray(Charsets.UTF_8))
            val first = if (isHost) null else handshakeSignal(noise.writeMessage() ?: return null)
            return E2eChannel(partner, isHost, noise, null) to first
        }

        /** Tests only: a channel that already has its transport keys. */
        internal fun withTransport(partner: String, isHost: Boolean, transport: NoiseXX.Transport) =
            E2eChannel(partner, isHost, null, transport)

        /** True for a `p2p-signal` payload of the end-to-end handshake. */
        fun isSignal(v: JsonObject): Boolean =
            v.get("v")?.takeIf { it.isJsonPrimitive }?.asString == "2" && (v.has("e2e") || v.has("e2e-ready"))

        private fun handshakeSignal(msg: ByteArray) = "{\"v\":2,\"e2e\":\"${Base64.toBase64String(msg)}\"}"

        private fun segments(len: Int): Long = (4L + len + SEGMENT - 1) / SEGMENT

        private fun sealSegments(t: NoiseXX.Transport, nonce: Long, msg: ByteArray, out: java.io.ByteArrayOutputStream) {
            val plain = ByteArray(4 + msg.size)
            writeInt(plain, 0, msg.size)
            System.arraycopy(msg, 0, plain, 4, msg.size)
            var i = 0
            var at = 0
            while (at < plain.size) {
                val len = minOf(SEGMENT, plain.size - at)
                out.write(t.encrypt(nonce + i, plain, at, len))
                at += len
                i++
            }
        }

        private fun isFramed(msg: ByteArray) =
            msg.size >= FRAMED_HEADER && (0 until 4).all { msg[it] == FRAMED_VIDEO[it] }

        private fun isAudio(msg: ByteArray) = msg.size > 1 && msg[0] == AUDIO_MARKER

        /** Stream and routing header length of a message that can be sealed. */
        private fun classifyPlain(msg: ByteArray): Pair<Stream, Int>? = when {
            isFramed(msg) -> Stream.VIDEO to FRAMED_HEADER
            isAudio(msg) -> Stream.AUDIO to 2
            else -> null
        }

        /** Stream and routing header length of a sealed message. */
        private fun classifySealed(msg: ByteArray): Pair<Stream, Int>? = when {
            isFramed(msg) && msg[7] == SEALED -> Stream.VIDEO to FRAMED_HEADER
            isAudio(msg) && msg[1] == SEALED -> Stream.AUDIO to 2
            else -> null
        }

        /** The stream a plain message would belong to: any JPEG or framed video, and voice. */
        private fun plainStream(msg: ByteArray): Stream? =
            if (msg.size > 2 && msg[0] == 0xFF.toByte() && msg[1] == 0xD8.toByte()) Stream.VIDEO else classifyPlain(msg)?.first

        /** Turns a plain routing header into its sealed form; the relay still reads the same routing bytes. */
        private fun mark(stream: Stream, header: ByteArray) {
            when (stream) {
                Stream.VIDEO -> header[7] = SEALED
                Stream.AUDIO -> header[1] = SEALED
                else -> {}
            }
        }

        private fun longBytes(v: Long) = ByteArray(8) { (v ushr (56 - 8 * it)).toByte() }

        private fun readLong(b: ByteArray, at: Int): Long {
            var v = 0L
            for (i in 0 until 8) v = (v shl 8) or (b[at + i].toLong() and 0xFF)
            return v
        }

        private fun readInt(b: ByteArray, at: Int): Int =
            ((b[at].toInt() and 0xFF) shl 24) or ((b[at + 1].toInt() and 0xFF) shl 16) or
                ((b[at + 2].toInt() and 0xFF) shl 8) or (b[at + 3].toInt() and 0xFF)

        private fun writeInt(b: ByteArray, at: Int, v: Int) {
            for (i in 0 until 4) b[at + i] = (v ushr (24 - 8 * i)).toByte()
        }
    }
}
