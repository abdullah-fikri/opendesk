package com.opendesk.app.crypto

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Interoperability with the desktop client: the vectors below were produced by `snow` 0.10 (the Noise library of
 * client-desktop) with static keys `01..01` (viewer) and `02..02` (host), ephemeral keys `03..03` and `04..04`
 * and the prologue of a session between 111111111 (viewer) and 222222222 (host).
 */
class NoiseXXTest {

    @Test
    fun handshakeAndTransportMatchSnowByteForByte() {
        val (viewer, host) = fixedPair()
        val m1 = viewer.writeMessage()!!
        assertArrayEquals(hex(M1), m1)
        assertTrue(host.readMessage(m1))
        val m2 = host.writeMessage()!!
        assertArrayEquals(hex(M2), m2)
        assertTrue(viewer.readMessage(m2))
        assertArrayEquals(hex(HOST_PUBLIC), viewer.remoteStatic)
        val m3 = viewer.writeMessage()!!
        assertArrayEquals(hex(M3), m3)
        assertTrue(host.readMessage(m3))
        assertArrayEquals(hex(VIEWER_PUBLIC), host.remoteStatic)
        assertTrue(viewer.isFinished && host.isFinished)

        val vt = viewer.transport!!
        val ht = host.transport!!
        val input = vt.encrypt((4L shl 56) or 7, "viewer input".toByteArray())
        assertArrayEquals(hex(T1), input)
        assertArrayEquals("viewer input".toByteArray(), ht.decrypt((4L shl 56) or 7, input))
        assertArrayEquals("host frame".toByteArray(), vt.decrypt((1L shl 56) or 9, hex(T2)))
    }

    @Test
    fun tamperedHandshakeOrTransportIsRejected() {
        val (viewer, host) = fixedPair()
        assertTrue(host.readMessage(viewer.writeMessage()!!))
        val m2 = host.writeMessage()!!
        m2[40] = (m2[40].toInt() xor 1).toByte()
        assertFalse(viewer.readMessage(m2))
        assertNull("a failed handshake writes nothing", viewer.writeMessage())

        val (v2, h2) = fixedPair()
        assertTrue(h2.readMessage(v2.writeMessage()!!))
        assertTrue(v2.readMessage(h2.writeMessage()!!))
        assertTrue(h2.readMessage(v2.writeMessage()!!))
        val sealed = h2.transport!!.encrypt(5, "x".toByteArray())
        sealed[0] = (sealed[0].toInt() xor 1).toByte()
        assertNull(v2.transport!!.decrypt(5, sealed))
        assertNull("wrong nonce", v2.transport!!.decrypt(6, h2.transport!!.encrypt(5, "x".toByteArray())))
    }

    @Test
    fun randomKeysCompleteTheHandshake() {
        val prologue = "p".toByteArray()
        val viewer = NoiseXX(true, NoiseXX.generatePrivateKey(), prologue)
        val host = NoiseXX(false, NoiseXX.generatePrivateKey(), prologue)
        assertFalse("the responder never writes first", host.isMyTurn)
        assertTrue(host.readMessage(viewer.writeMessage()!!))
        assertTrue(viewer.readMessage(host.writeMessage()!!))
        assertTrue(host.readMessage(viewer.writeMessage()!!))
        assertArrayEquals(byteArrayOf(9), host.transport!!.decrypt(1, viewer.transport!!.encrypt(1, byteArrayOf(9))))
    }

    companion object {
        const val VIEWER_PUBLIC = "a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209"
        const val HOST_PUBLIC = "ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59"
        const val M1 = "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22"
        const val M2 = "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10ba008df889bb35e8223335723afaf9154" +
            "defd8da5df240cc3592090c2fb4518e08456c4c0217dd3dab4e8db0bc56f8fcb4cd1a61de2784b5be5aae54d4e736e73"
        const val M3 = "b7cdf8c991735a27cbc64fb59293bea96330d41179052f565b352ffad9b79b8778a28de2e1f6a72857bc217357dca457" +
            "4726dfca2909f3d90064159746e269a2"
        const val T1 = "dd9f7cb3cbeb738968d4f408c439f6ed2fcc9982722d25a380388207"
        const val T2 = "8c717e4909cd04e6e9bb5d2a7dd29ed2e55efb710ce65e0e08ce"

        val PROLOGUE = "opendesk-e2e-v1|111111111|222222222".toByteArray()

        /** The fixed viewer/host pair of the vectors. */
        fun fixedPair(): Pair<NoiseXX, NoiseXX> = Pair(
            NoiseXX(true, ByteArray(32) { 1 }, PROLOGUE, fixedEphemeral = ByteArray(32) { 3 }),
            NoiseXX(false, ByteArray(32) { 2 }, PROLOGUE, fixedEphemeral = ByteArray(32) { 4 }),
        )

        fun hex(s: String): ByteArray = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }
    }
}
