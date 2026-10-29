package com.opendesk.app.crypto

import org.bouncycastle.crypto.digests.Blake2sDigest
import org.bouncycastle.crypto.macs.HMac
import org.bouncycastle.crypto.modes.ChaCha20Poly1305
import org.bouncycastle.crypto.params.AEADParameters
import org.bouncycastle.crypto.params.KeyParameter
import org.bouncycastle.math.ec.rfc7748.X25519
import java.security.SecureRandom

/**
 * `Noise_XX_25519_ChaChaPoly_BLAKE2s` (Noise Protocol Framework, revision 34) with empty payloads, as the desktop
 * client runs it with `snow` (client-desktop/src/infra/e2e.rs). Only the primitives come from BouncyCastle; the
 * state machine follows the specification section by section. Interoperability with `snow` is pinned by test
 * vectors in `NoiseXXTest`.
 *
 * Message pattern: `-> e` / `<- e, ee, s, es` / `-> s, se`. The viewer is the initiator.
 */
class NoiseXX(
    private val initiator: Boolean,
    private val staticPrivate: ByteArray,
    prologue: ByteArray,
    /** Tests only: a fixed ephemeral key, to reproduce a known transcript. */
    private val fixedEphemeral: ByteArray? = null,
) {
    private val sym = SymmetricState(PROTOCOL_NAME.toByteArray(Charsets.US_ASCII))
    private val staticPublic = publicKey(staticPrivate)
    private var ephemeralPrivate: ByteArray? = null
    private var remoteEphemeral: ByteArray? = null

    /** The peer's static key, once its handshake message carrying it was read. */
    var remoteStatic: ByteArray? = null
        private set

    /** Number of handshake messages written or read so far (0..3). */
    private var step = 0
    private var failed = false

    /** Transport keys once all three messages went through. */
    var transport: Transport? = null
        private set

    init {
        sym.mixHash(prologue)
    }

    val isFinished: Boolean get() = transport != null

    /** Whether the next handshake message is ours to write. */
    val isMyTurn: Boolean get() = !failed && step < 3 && (step % 2 == 0) == initiator

    /** Writes the next handshake message; null when it is not this side's turn or the handshake failed. */
    fun writeMessage(): ByteArray? {
        if (!isMyTurn) return null
        return runCatching {
            val out = when (step) {
                0 -> writeEphemeral() + sym.encryptAndHash(EMPTY)
                1 -> {
                    val e = writeEphemeral()
                    sym.mixKey(dh(ephemeralPrivate!!, remoteEphemeral!!)) // ee
                    val s = sym.encryptAndHash(staticPublic)
                    sym.mixKey(dh(staticPrivate, remoteEphemeral!!)) // es (responder side)
                    e + s + sym.encryptAndHash(EMPTY)
                }
                else -> {
                    val s = sym.encryptAndHash(staticPublic)
                    sym.mixKey(dh(staticPrivate, remoteEphemeral!!)) // se (initiator side)
                    s + sym.encryptAndHash(EMPTY)
                }
            }
            advance()
            out
        }.getOrElse {
            failed = true
            null
        }
    }

    /** Reads the peer's next handshake message; false (and the handshake is dead) when it does not verify. */
    fun readMessage(message: ByteArray): Boolean {
        if (failed || step >= 3 || isMyTurn) return false
        val ok = runCatching {
            when (step) {
                0 -> {
                    require(message.size == DH_LEN)
                    readEphemeral(message)
                    sym.decryptAndHash(EMPTY)
                }
                1 -> {
                    require(message.size == DH_LEN + DH_LEN + TAG + TAG)
                    readEphemeral(message.copyOfRange(0, DH_LEN))
                    sym.mixKey(dh(ephemeralPrivate!!, remoteEphemeral!!)) // ee
                    remoteStatic = sym.decryptAndHash(message.copyOfRange(DH_LEN, DH_LEN + DH_LEN + TAG))
                    sym.mixKey(dh(ephemeralPrivate!!, remoteStatic!!)) // es (initiator side)
                    sym.decryptAndHash(message.copyOfRange(DH_LEN + DH_LEN + TAG, message.size))
                }
                else -> {
                    require(message.size == DH_LEN + TAG + TAG)
                    remoteStatic = sym.decryptAndHash(message.copyOfRange(0, DH_LEN + TAG))
                    sym.mixKey(dh(ephemeralPrivate!!, remoteStatic!!)) // se (responder side)
                    sym.decryptAndHash(message.copyOfRange(DH_LEN + TAG, message.size))
                }
            }
            advance()
        }.isSuccess
        if (!ok) failed = true
        return ok
    }

    private fun writeEphemeral(): ByteArray {
        val e = fixedEphemeral?.copyOf() ?: ByteArray(DH_LEN).also { random.nextBytes(it) }
        ephemeralPrivate = e
        val pub = publicKey(e)
        sym.mixHash(pub)
        return pub
    }

    private fun readEphemeral(pub: ByteArray) {
        remoteEphemeral = pub
        sym.mixHash(pub)
    }

    private fun advance() {
        step++
        if (step == 3) {
            val (k1, k2) = sym.split()
            transport = if (initiator) Transport(send = k1, receive = k2) else Transport(send = k2, receive = k1)
            ephemeralPrivate?.fill(0)
        }
    }

    /**
     * The two transport keys, used statelessly: every message names its 64-bit nonce, so independent streams
     * (and messages the relay reorders between streams) never share a counter.
     */
    class Transport internal constructor(private val send: ByteArray, private val receive: ByteArray) {
        /** Encrypts `plain` (at most [MAX_PLAINTEXT] bytes) with nonce `n`. */
        fun encrypt(n: Long, plain: ByteArray, offset: Int = 0, length: Int = plain.size): ByteArray =
            aead(true, send, n, EMPTY, plain, offset, length)

        /** Decrypts and authenticates; null when the message was forged or damaged. */
        fun decrypt(n: Long, sealed: ByteArray, offset: Int = 0, length: Int = sealed.size): ByteArray? =
            runCatching { aead(false, receive, n, EMPTY, sealed, offset, length) }.getOrNull()
    }

    private class SymmetricState(protocolName: ByteArray) {
        private var h: ByteArray = if (protocolName.size <= HASH_LEN) protocolName.copyOf(HASH_LEN) else hash(protocolName)
        private var ck: ByteArray = h.copyOf()
        private var k: ByteArray? = null
        private var n = 0L

        fun mixHash(data: ByteArray) {
            h = hash(h + data)
        }

        fun mixKey(ikm: ByteArray) {
            val (newCk, tempK) = hkdf(ck, ikm)
            ck = newCk
            k = tempK
            n = 0
        }

        fun encryptAndHash(plain: ByteArray): ByteArray {
            val key = k
            val out = if (key == null) plain.copyOf() else aead(true, key, n++, h, plain, 0, plain.size)
            mixHash(out)
            return out
        }

        fun decryptAndHash(data: ByteArray): ByteArray {
            val key = k
            val out = if (key == null) data.copyOf() else aead(false, key, n++, h, data, 0, data.size)
            mixHash(data)
            return out
        }

        fun split(): Pair<ByteArray, ByteArray> = hkdf(ck, EMPTY)
    }

    companion object {
        const val PROTOCOL_NAME = "Noise_XX_25519_ChaChaPoly_BLAKE2s"
        const val DH_LEN = 32
        const val TAG = 16

        /** Largest plaintext of one transport message (65535 minus the tag). */
        const val MAX_PLAINTEXT = 65535 - TAG
        private const val HASH_LEN = 32
        private val EMPTY = ByteArray(0)
        private val random = SecureRandom()

        /** A fresh X25519 private key. */
        fun generatePrivateKey(): ByteArray = ByteArray(DH_LEN).also { random.nextBytes(it) }

        /** The X25519 public key of `privateKey`. */
        fun publicKey(privateKey: ByteArray): ByteArray =
            ByteArray(DH_LEN).also { X25519.generatePublicKey(privateKey, 0, it, 0) }

        private fun dh(privateKey: ByteArray, publicKey: ByteArray): ByteArray {
            val out = ByteArray(DH_LEN)
            // False for a low-order peer key (all-zero shared secret): never accept it.
            check(X25519.calculateAgreement(privateKey, 0, publicKey, 0, out, 0)) { "invalid peer key" }
            return out
        }

        private fun hash(data: ByteArray): ByteArray {
            val d = Blake2sDigest()
            d.update(data, 0, data.size)
            return ByteArray(HASH_LEN).also { d.doFinal(it, 0) }
        }

        private fun hmac(key: ByteArray, data: ByteArray): ByteArray {
            val mac = HMac(Blake2sDigest())
            mac.init(KeyParameter(key))
            mac.update(data, 0, data.size)
            return ByteArray(HASH_LEN).also { mac.doFinal(it, 0) }
        }

        /** Noise HKDF with two outputs. */
        private fun hkdf(chainingKey: ByteArray, ikm: ByteArray): Pair<ByteArray, ByteArray> {
            val temp = hmac(chainingKey, ikm)
            val out1 = hmac(temp, byteArrayOf(1))
            val out2 = hmac(temp, out1 + byteArrayOf(2))
            return out1 to out2
        }

        /** ChaCha20-Poly1305 (RFC 8439) with the Noise nonce: 4 zero bytes, then the counter little-endian. */
        private fun aead(
            encrypt: Boolean,
            key: ByteArray,
            n: Long,
            ad: ByteArray,
            data: ByteArray,
            offset: Int,
            length: Int,
        ): ByteArray {
            val nonce = ByteArray(12)
            for (i in 0 until 8) nonce[4 + i] = (n ushr (8 * i)).toByte()
            val cipher = ChaCha20Poly1305()
            cipher.init(encrypt, AEADParameters(KeyParameter(key), TAG * 8, nonce, ad))
            val out = ByteArray(cipher.getOutputSize(length))
            val written = cipher.processBytes(data, offset, length, out, 0)
            val total = written + cipher.doFinal(out, written)
            return if (total == out.size) out else out.copyOf(total)
        }
    }
}
