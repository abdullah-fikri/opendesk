package com.opendesk.app.domain

import com.opendesk.app.domain.FramedVideo.Rect
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class FramedVideoTest {

    /** A message as core-engine/src/codec/frame.rs `FrameHeader::encode` writes it. */
    private fun encode(kind: Int, seq: Long, w: Int, h: Int, moves: List<IntArray>, patches: List<IntArray>, payload: ByteArray): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        fun u16(v: Int) = out.write(byteArrayOf((v shr 8).toByte(), v.toByte()))
        out.write(byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0, 0x4F, 1, kind.toByte(), 0, 0))
        out.write(byteArrayOf((seq shr 24).toByte(), (seq shr 16).toByte(), (seq shr 8).toByte(), seq.toByte()))
        u16(w); u16(h); u16(moves.size); u16(patches.size)
        (moves + patches).forEach { r -> r.forEach(::u16) }
        out.write(payload)
        return out.toByteArray()
    }

    @Test
    fun headerParsesMovesPatchesAndPayload() {
        val msg = encode(
            FramedVideo.KIND_DELTA, 0xFFFF_FFFEL, 1920, 1080,
            moves = listOf(intArrayOf(0, 10, 0, 0, 1920, 500)),
            // dst_x, dst_y, w, h, atlas_x, atlas_y
            patches = listOf(intArrayOf(100, 200, 32, 16, 64, 0)),
            payload = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte()),
        )
        val f = FramedVideo.parse(msg)!!
        assertEquals(FramedVideo.KIND_DELTA, f.kind)
        assertEquals(0xFFFF_FFFEL, f.seq)
        assertEquals(1920, f.width)
        assertEquals(1080, f.height)
        assertEquals(listOf(Rect(0, 10, 0, 0, 1920, 500)), f.moves)
        assertEquals(listOf(Rect(srcX = 64, srcY = 0, dstX = 100, dstY = 200, w = 32, h = 16)), f.patches)
        assertEquals(msg.size - 3, f.payloadOffset)
    }

    @Test
    fun legacyTruncatedAndUnknownVersionsAreNotParsed() {
        val jpeg = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xE0.toByte(), 0, 0, 0, 0, 0, 0)
        assertFalse(FramedVideo.isFramed(jpeg))
        assertNull(FramedVideo.parse(jpeg))
        val msg = encode(FramedVideo.KIND_DELTA, 1, 4, 4, listOf(intArrayOf(0, 0, 1, 1, 2, 2)), emptyList(), ByteArray(0))
        assertTrue(FramedVideo.isFramed(msg))
        assertNull("truncated rects", FramedVideo.parse(msg.copyOf(msg.size - 1)))
        assertNull("version 2", FramedVideo.parse(msg.copyOf().also { it[4] = 2 }))
    }

    @Test
    fun sequenceWrapsAndClippingStaysInsideBothImages() {
        assertTrue(FramedVideo.follows(0xFFFF_FFFFL, 0))
        assertTrue(FramedVideo.follows(5, 6))
        assertFalse(FramedVideo.follows(5, 7))
        assertFalse("no base frame", FramedVideo.follows(null, 1))

        // A patch hanging over the right edge of the frame and the bottom of the atlas.
        val r = Rect(srcX = 60, srcY = 30, dstX = 90, dstY = 0, w = 32, h = 32)
        assertEquals(r.copy(w = 10, h = 2), FramedVideo.clip(r, srcW = 128, srcH = 32, dstW = 100, dstH = 100))
        assertNull("entirely outside", FramedVideo.clip(r.copy(dstX = 120), 128, 32, 100, 100))
        assertNull("source beyond the atlas", FramedVideo.clip(r.copy(srcX = 500), 128, 32, 100, 100))
    }
}
