package com.opendesk.app.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MonitorWallTest {
    private fun host(id: String, name: String, online: Boolean) = WallHost(id, name, online)

    @Test
    fun selectionAllShowsOnlineDevicesAndPickedShowsOfflineOnesToo() {
        val hosts = listOf(host("3", "beta", true), host("1", "Alpha", false), host("2", "alpha", true))
        assertEquals(listOf("2", "3"), MonitorWall.wallHosts(hosts, MonitorSettings(3, all = true)).map { it.id })
        val picked = MonitorWall.wallHosts(hosts, MonitorSettings(3, all = false, selected = setOf("3", "1", "9")))
        assertEquals("hand-picked, offline included, unknown IDs skipped, name order", listOf("1", "3"), picked.map { it.id })
        assertTrue(MonitorWall.wallHosts(hosts, MonitorSettings(3, all = false)).isEmpty())
    }

    @Test
    fun togglingAHostAddsAndRemovesIt() {
        var s = emptySet<String>()
        s = MonitorWall.toggle(s, "123456789")
        s = MonitorWall.toggle(s, "222222222")
        s = MonitorWall.toggle(s, "123456789")
        assertEquals(setOf("222222222"), s)
    }

    @Test
    fun pagesSplitByGridAndClamp() {
        for ((g, n) in listOf(1 to 1, 2 to 4, 3 to 4, 4 to 4, 0 to 1, 9 to 4)) {
            assertEquals("grid $g", n, MonitorWall.perPage(MonitorSettings(g)))
        }
        val list = (0 until 20).toList()
        assertEquals(2, MonitorWall.pageCount(20, 16))
        assertEquals(1, MonitorWall.pageCount(0, 16))
        assertEquals(1, MonitorWall.pageCount(16, 16))
        assertEquals(16, MonitorWall.pageOf(list, 16, 0).size)
        assertEquals(listOf(16, 17, 18, 19), MonitorWall.pageOf(list, 16, 1))
        assertEquals("past the end clamps to the last page", listOf(16, 17, 18, 19), MonitorWall.pageOf(list, 16, 7))
        assertTrue(MonitorWall.pageOf(emptyList<Int>(), 9, 3).isEmpty())
        assertEquals(listOf(18, 19), MonitorWall.pageOf(list, 9, 2))
    }

    @Test
    fun tilesKeepTheChosenColumnCount() {
        // 3x3 on a tall phone: three columns, width-limited.
        assertEquals((1080 - 2 * 6) / 3f, MonitorWall.tileWidth(1080, 2000, 3, 9, 6), 0.01f)
        // 3x3 on a landscape phone: height-limited, still three columns.
        assertEquals((1000 - 2 * 6) / 3f * MonitorWall.TILE_ASPECT, MonitorWall.tileWidth(2200, 1000, 3, 9, 6), 0.01f)
        // More columns than tiles: as many as there are tiles.
        assertEquals((1000 - 6) / 2f, MonitorWall.tileWidth(1000, 4000, 4, 2, 6), 0.01f)
    }

    @Test
    fun pageStepsWrap() {
        assertEquals(1, MonitorWall.stepPage(0, 1, 3))
        assertEquals(0, MonitorWall.stepPage(2, 1, 3))
        assertEquals(2, MonitorWall.stepPage(0, -1, 3))
        assertEquals(0, MonitorWall.stepPage(5, 1, 1))
        assertEquals(0, MonitorWall.stepPage(0, 1, 0))
    }

    @Test
    fun rotationAdvancesOnItsIntervalOnlyWithSeveralPages() {
        var s = MonitorSettings(3, rotate = true, rotateSecs = 10)
        val r = Rotation(0)
        val every = MonitorWall.rotateIntervalMs(s)
        assertFalse(r.due(every, 3, 9_000))
        assertTrue(r.due(every, 3, 10_000))
        assertFalse("countdown restarted", r.due(every, 3, 15_000))
        assertTrue(r.due(every, 3, 20_000))
        assertFalse("one page never rotates", r.due(every, 1, 60_000))
        r.reset(60_000)
        assertFalse("a manual change restarts the countdown", r.due(every, 3, 65_000))
        s = s.copy(rotateSecs = 1)
        assertEquals(MonitorWall.ROTATE_MIN_SECS * 1000L, MonitorWall.rotateIntervalMs(s))
        assertNull(MonitorWall.rotateIntervalMs(s.copy(rotate = false)))
        assertFalse(r.due(null, 3, 3_600_000))
    }

    @Test
    fun thumbnailsParseOnlyWithADigitIdAndAPlainJpeg() {
        val jpeg = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 1, 2, 3)
        val ok = byteArrayOf(0x11) + "123456789".toByteArray() + jpeg
        val t = Thumbnail.parse(ok)!!
        assertEquals("123456789", t.hostClean)
        assertEquals(10, t.offset)
        assertEquals(jpeg.size, t.length)

        assertNull("letters in the ID", Thumbnail.parse(byteArrayOf(0x11) + "12345678x".toByteArray() + jpeg))
        assertNull("framed video is not a thumbnail", Thumbnail.parse(byteArrayOf(0x11) + "123456789".toByteArray() + byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0, 0x4F)))
        assertNull("too short", Thumbnail.parse(byteArrayOf(0x11, '1'.code.toByte())))
        assertNull("other tag", Thumbnail.parse(byteArrayOf(0x10) + "123456789".toByteArray() + jpeg))
        assertNull("oversized", Thumbnail.parse(byteArrayOf(0x11) + "123456789".toByteArray() + jpeg + ByteArray(Thumbnail.MAX_JPEG)))
        assertFalse("sealed video", Thumbnail.isTagged(byteArrayOf(0xFF.toByte(), 0xD8.toByte())))
        assertFalse("voice", Thumbnail.isTagged(byteArrayOf(0x02, 0xE2.toByte())))
    }
}
