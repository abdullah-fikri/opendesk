package com.opendesk.app.session

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import com.opendesk.app.crypto.NoiseXX
import com.opendesk.app.crypto.NoiseXXTest
import com.opendesk.app.crypto.NoiseXXTest.Companion.hex
import org.bouncycastle.util.encoders.Base64
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import java.security.MessageDigest

/**
 * The sealed wire format against the desktop client: the vectors were produced by client-desktop/src/infra/e2e.rs
 * with the transport keys of [NoiseXXTest.fixedPair].
 */
class E2eChannelTest {

    private fun fixedChannels(): Pair<E2eChannel, E2eChannel> {
        val (v, h) = NoiseXXTest.fixedPair()
        h.readMessage(v.writeMessage()!!)
        v.readMessage(h.writeMessage()!!)
        h.readMessage(v.writeMessage()!!)
        return E2eChannel.withTransport("222222222", false, v.transport!!) to
            E2eChannel.withTransport("111111111", true, h.transport!!)
    }

    private fun framed(kind: Int, len: Int) =
        byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0, 0x4F, 1, kind.toByte(), 0, 0) + ByteArray(len) { it.toByte() }

    @Test
    fun sealedMessagesMatchTheDesktopByteForByte() {
        val (viewer, host) = fixedChannels()
        // Host to viewer: a delta frame, then a key frame larger than one Noise message, then voice.
        val small = host.seal(framed(2, 40))!!
        assertArrayEquals(hex(SMALL), small)
        val big = host.seal(framed(1, 70_000))!!
        assertEquals(70_060, big.size)
        assertEquals(BIG_SHA, MessageDigest.getInstance("SHA-256").digest(big).joinToString("") { "%02x".format(it) })
        assertArrayEquals(hex(VOICE_IN), host.seal(byteArrayOf(2, 16, 9, 8, 7, 6)))

        assertArrayEquals(framed(2, 40), viewer.open(hex(SMALL)))
        assertArrayEquals(framed(1, 70_000), viewer.open(big))
        assertArrayEquals(byteArrayOf(2, 16, 9, 8, 7, 6), viewer.open(hex(VOICE_IN)))

        // Viewer to host: voice and an input event.
        assertArrayEquals(hex(VOICE_OUT), viewer.seal(byteArrayOf(2, 16, 1, 2, 3, 4)))
        val sealed = viewer.sealEvent("{\"action\":\"keydown\",\"keyCode\":65}")!!
        assertEquals("e2e", sealed.get("action").asString)
        assertArrayEquals(hex(EVENT_OUT), Base64.decode(sealed.get("d").asString))

        val fromHost = JsonObject().apply {
            addProperty("action", "e2e")
            addProperty("d", Base64.toBase64String(hex(EVENT_IN)))
        }
        assertEquals(JsonParser.parseString("{\"action\":\"set-mic\",\"muted\":true}"), viewer.openEvent(fromHost))
    }

    @Test
    fun replayedTamperedAndInjectedPlainMessagesAreDropped() {
        val (viewer, _) = fixedChannels()
        val plainFrame = framed(2, 10)
        assertSame("plain passes before anything is sealed", plainFrame, viewer.open(plainFrame))
        val tampered = hex(SMALL).also { it[it.size - 1] = (it[it.size - 1].toInt() xor 1).toByte() }
        assertNull(viewer.open(tampered))
        val relabeled = hex(SMALL).also { it[5] = 1 } // a delta disguised as a key frame
        assertNull(viewer.open(relabeled))
        assertNotNull(viewer.open(hex(SMALL)))
        assertNull("replay", viewer.open(hex(SMALL)))
        assertNull("plain video injected once video is sealed", viewer.open(plainFrame))
        assertNull("legacy JPEG too", viewer.open(byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xE0.toByte())))
        val pcm = byteArrayOf(2, 16, 0, 0)
        assertSame("voice is still plain from this host", pcm, viewer.open(pcm))

        val plainEvent = JsonParser.parseString("{\"action\":\"set-mic\",\"muted\":true}").asJsonObject
        assertSame(plainEvent, viewer.openEvent(plainEvent))
        val sealedEvent = JsonObject().apply {
            addProperty("action", "e2e")
            addProperty("d", Base64.toBase64String(hex(EVENT_IN)))
        }
        assertNotNull(viewer.openEvent(sealedEvent))
        assertNull("replay", viewer.openEvent(sealedEvent))
        assertNull("plain event injected after the switch", viewer.openEvent(plainEvent))
    }

    @Test
    fun handshakeOverSignalsSealsOnlyAfterTheHostIsReady() {
        val (viewer, first) = E2eChannel.start(false, "111111111", "222222222", NoiseXX.generatePrivateKey())!!
        val (host, none) = E2eChannel.start(true, "222222222", "111111111", NoiseXX.generatePrivateKey())!!
        assertNull(none)
        val parse = { s: String -> JsonParser.parseString(s).asJsonObject }
        val msg1 = parse(first!!)
        assertTrue(E2eChannel.isSignal(msg1))
        val msg2 = parse(host.onSignal(msg1) { true }!!)
        var seen: ByteArray? = null
        val msg3 = parse(viewer.onSignal(msg2) { seen = it; true }!!)
        assertArrayEquals(seen, viewer.peerKey)
        val ready = parse(host.onSignal(msg3) { true }!!)
        assertTrue(E2eChannel.isSignal(ready))
        assertTrue(host.isOn)
        assertFalse("viewer waits for the host's confirmation", viewer.isOn)
        assertNull(viewer.seal(byteArrayOf(2, 16, 1)))
        assertNull(viewer.onSignal(ready) { true })
        assertTrue(viewer.isOn)
        val event = viewer.sealEvent("{\"action\":\"click\"}")!!
        assertEquals("click", host.openEvent(event)!!.get("action").asString)
        assertArrayEquals(framed(1, 5), viewer.open(host.seal(framed(1, 5))!!))
    }

    @Test
    fun rejectedHostKeyLeavesBothSidesPlain() {
        val (viewer, first) = E2eChannel.start(false, "111111111", "222222222", NoiseXX.generatePrivateKey())!!
        val (host, _) = E2eChannel.start(true, "222222222", "111111111", NoiseXX.generatePrivateKey())!!
        val parse = { s: String -> JsonParser.parseString(s).asJsonObject }
        val msg2 = parse(host.onSignal(parse(first!!)) { true }!!)
        assertNull(viewer.onSignal(msg2) { false })
        assertTrue(viewer.hasFailed)
        assertFalse(viewer.isOn || host.isOn)
        assertNull(host.seal(framed(1, 5)))
    }

    private companion object {
        const val SMALL = "ffd8004f010200e201000000000000003ef318007f8f80b984bad85c2083f1e17f00c01aa04891fdb2ac32885110bc" +
            "9c58cc07997dfe1812a3c8a57eb5f156d645c980f73547ced908782820bfdcaa5a246d339e"
        const val BIG_SHA = "44359431306415de3c48b20768af3300f74915f2ce1b8018a7c260c2cf1199d0"
        const val VOICE_IN = "02e20200000000000000c3cf8047f5cba616deaca12fe5622e35c4e2378c94df710fcf19"
        const val VOICE_OUT = "02e2020000000000000042a380c6d6910fa5fb7d21922a5200d167e52eadaf0f29fd49a5"
        const val EVENT_OUT = "040000000000000014819d79b85d330bec3683ec405cb10f06b6007ced7081bbe4fc1f29bd760c5fa05a59ef39ddbf" +
            "28b32deda2a0bbdb3b76677aa8ce"
        const val EVENT_IN = "0400000000000000202f7f5aee2e00951f2ae926bf3a71b757e6e68efa0e9539abf14812b99eae413a8482a217f8" +
            "a7617c57484d67164c57449145c490"
    }
}
