package com.opendesk.app.domain

/**
 * Framed video (`video-delta`, docs/protocol.md#binary-messages at the repository root), all integers big-endian:
 *
 * ```
 * 0   FF D8 00 4F   magic (starts like a JPEG; 0x00 never follows SOI in a real JPEG)
 * 4   version = 1
 * 5   kind: 1 key (payload = JPEG), 2 delta (payload = JPEG atlas), 3 H.264 access unit (Annex-B)
 * 6   flags: bit 0 = H.264 IDR (SPS/PPS in band), bit 1 = stamped, bit 2 = lossless tiles
 * 7   reserved
 * 8   seq (u32): a delta applies only on top of seq-1
 * 12  width, height (u16)
 * 16  move count, patch count (u16)
 * 20  moves   (src_x, src_y, dst_x, dst_y, w, h) x u16, applied first
 *     patches (dst_x, dst_y, w, h, atlas_x, atlas_y) x u16, blitted from the payload
 *     payload
 * ```
 */
object FramedVideo {
    const val KIND_KEY = 1
    const val KIND_DELTA = 2
    const val KIND_H264 = 3

    /** Flag: the H.264 access unit is an IDR, so a decoder may start here. */
    const val FLAG_IDR = 1
    private const val FIXED_HEADER = 20
    private const val RECT_BYTES = 12

    /** A region copied inside the frame, or blitted from the atlas (`srcX/srcY` are atlas coordinates then). */
    data class Rect(val srcX: Int, val srcY: Int, val dstX: Int, val dstY: Int, val w: Int, val h: Int)

    data class Frame(
        val kind: Int,
        val flags: Int,
        val seq: Long,
        val width: Int,
        val height: Int,
        val moves: List<Rect>,
        val patches: List<Rect>,
        val payloadOffset: Int,
    )

    /** True for a framed message (as opposed to a legacy plain JPEG). */
    fun isFramed(msg: ByteArray): Boolean =
        msg.size >= 8 && msg[0] == 0xFF.toByte() && msg[1] == 0xD8.toByte() && msg[2] == 0.toByte() && msg[3] == 0x4F.toByte()

    /** Parses the header; null when it is truncated or not version 1. */
    fun parse(msg: ByteArray): Frame? {
        if (!isFramed(msg) || msg.size < FIXED_HEADER || msg[4].toInt() != 1) return null
        val moveCount = u16(msg, 16)
        val patchCount = u16(msg, 18)
        val payload = FIXED_HEADER + (moveCount + patchCount) * RECT_BYTES
        if (msg.size < payload) return null
        val moves = (0 until moveCount).map { i ->
            val at = FIXED_HEADER + i * RECT_BYTES
            Rect(u16(msg, at), u16(msg, at + 2), u16(msg, at + 4), u16(msg, at + 6), u16(msg, at + 8), u16(msg, at + 10))
        }
        val patches = (0 until patchCount).map { i ->
            val at = FIXED_HEADER + (moveCount + i) * RECT_BYTES
            // On the wire: dst_x, dst_y, w, h, atlas_x, atlas_y.
            Rect(u16(msg, at + 8), u16(msg, at + 10), u16(msg, at), u16(msg, at + 2), u16(msg, at + 4), u16(msg, at + 6))
        }
        val seq = ((u16(msg, 8).toLong() shl 16) or u16(msg, 10).toLong())
        return Frame(msg[5].toInt(), msg[6].toInt() and 0xFF, seq, u16(msg, 12), u16(msg, 14), moves, patches, payload)
    }

    /** True when a delta numbered `seq` applies on top of frame `last` (u32 wrap-around). */
    fun follows(last: Long?, seq: Long): Boolean = last != null && ((last + 1) and 0xFFFF_FFFFL) == seq

    /**
     * Clips `r` against a source of `srcW` x `srcH` and a destination of `dstW` x `dstH`, so a malformed rect can
     * never read or write outside either image. Null when nothing is left.
     */
    fun clip(r: Rect, srcW: Int, srcH: Int, dstW: Int, dstH: Int): Rect? {
        val w = minOf(r.w, srcW - r.srcX, dstW - r.dstX)
        val h = minOf(r.h, srcH - r.srcY, dstH - r.dstY)
        return if (w > 0 && h > 0) r.copy(w = w, h = h) else null
    }

    private fun u16(b: ByteArray, at: Int): Int = ((b[at].toInt() and 0xFF) shl 8) or (b[at + 1].toInt() and 0xFF)

    /** True for an H.264 access unit a decoder may start from. */
    fun isIdr(f: Frame): Boolean = f.kind == KIND_H264 && (f.flags and FLAG_IDR) != 0

    /**
     * The SPS and PPS of an Annex-B access unit in `msg[from, until)`, each with its start code (MediaCodec's
     * `csd-0` / `csd-1`); null when either is missing. The host puts both in front of every IDR.
     */
    fun parameterSets(msg: ByteArray, from: Int, until: Int = msg.size): Pair<ByteArray, ByteArray>? {
        var sps: ByteArray? = null
        var pps: ByteArray? = null
        val starts = mutableListOf<Int>() // index of each NAL's first byte after the start code
        var i = from
        while (i + 3 <= until) {
            if (msg[i] == 0.toByte() && msg[i + 1] == 0.toByte() && msg[i + 2] == 1.toByte()) {
                starts.add(i + 3)
                i += 3
            } else {
                i++
            }
        }
        for ((n, start) in starts.withIndex()) {
            if (start >= until) continue
            // The NAL ends where the next start code begins (dropping the zero of a 4-byte start code).
            var end = if (n + 1 < starts.size) starts[n + 1] - 3 else until
            if (end > start && n + 1 < starts.size && msg[end - 1] == 0.toByte()) end--
            val type = msg[start].toInt() and 0x1F
            val nal = byteArrayOf(0, 0, 0, 1) + msg.copyOfRange(start, end)
            if (type == NAL_SPS && sps == null) sps = nal
            if (type == NAL_PPS && pps == null) pps = nal
        }
        return if (sps != null && pps != null) sps to pps else null
    }

    private const val NAL_SPS = 7
    private const val NAL_PPS = 8
}
