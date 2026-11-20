package com.opendesk.app.domain

/*
 * Monitor-wall rules (admin edition, see docs/features/monitor-wall.md at the repository root): which hosts the
 * wall shows, how they split into pages, when the page rotates, and the thumbnail framing. Pure data and
 * arithmetic, ported from the desktop's `domain/monitor_wall.rs`.
 */

/** One device the wall may show. */
data class WallHost(val id: String, val name: String, val online: Boolean)

/** The wall's settings. [grid] is tiles per row and column (1..4). */
data class MonitorSettings(
    val grid: Int,
    val rotate: Boolean = false,
    val rotateSecs: Int = MonitorWall.ROTATE_DEFAULT_SECS,
    /** Every online device; otherwise the [selected] ones, offline included. */
    val all: Boolean = true,
    val selected: Set<String> = emptySet(),
)

/** What one tile shows, from the relay's `monitor-state`. */
object MonitorState {
    const val LIVE = "live"
    const val OFFLINE = "offline"
    const val UNSUPPORTED = "unsupported"
    const val DENIED = "denied"
}

object MonitorWall {
    /** Most hosts one `monitor-set` may carry (the relay ignores the rest). */
    const val MAX_TILES = 16
    const val ROTATE_MIN_SECS = 5
    const val ROTATE_MAX_SECS = 600
    const val ROTATE_DEFAULT_SECS = 10

    /** Tiles per row and column, 1..4. */
    fun grid(settings: MonitorSettings): Int = settings.grid.coerceIn(1, 4)

    /** Tiles on one page (1, 4, 9 or 16). */
    fun perPage(settings: MonitorSettings): Int = grid(settings).let { it * it }

    /** Page interval in milliseconds, or null when rotation is off. */
    fun rotateIntervalMs(settings: MonitorSettings): Long? =
        if (settings.rotate) settings.rotateSecs.coerceIn(ROTATE_MIN_SECS, ROTATE_MAX_SECS) * 1000L else null

    /**
     * The hosts on the wall in a stable order (name, then ID), so pages do not reshuffle when a device comes or goes:
     * every online device in "all" mode, else the hand-picked ones, offline included so a missing machine shows.
     */
    fun wallHosts(hosts: List<WallHost>, settings: MonitorSettings): List<WallHost> =
        hosts.filter { if (settings.all) it.online else it.id in settings.selected }
            .sortedWith(compareBy({ it.name.lowercase() }, { it.id }))

    /** Number of pages for `n` hosts (at least one, even when empty). */
    fun pageCount(n: Int, perPage: Int): Int {
        val per = perPage.coerceAtLeast(1)
        return ((n + per - 1) / per).coerceAtLeast(1)
    }

    /** The items on `page` (clamped to the last page). */
    fun <T> pageOf(list: List<T>, perPage: Int, page: Int): List<T> {
        val per = perPage.coerceAtLeast(1)
        val p = page.coerceIn(0, pageCount(list.size, per) - 1)
        val start = (p * per).coerceAtMost(list.size)
        return list.subList(start, (start + per).coerceAtMost(list.size))
    }

    /** `page` moved by `delta`, wrapping around both ends. */
    fun stepPage(page: Int, delta: Int, pages: Int): Int = Math.floorMod(page + delta, pages.coerceAtLeast(1))

    /** Width:height of a tile; desktop screens are mostly 16:10 or 16:9 and thumbnails are letterboxed inside. */
    const val TILE_ASPECT = 1.6f

    /**
     * Width of each tile when `n` tiles sit in `columns` columns (as many rows as needed) inside `w` x `h`, `gap`
     * apart: as wide as the columns allow, smaller when the rows would not fit the height. The column count is the
     * user's grid setting, never picked here, so a 3x3 wall shows three columns on a tall phone too.
     */
    fun tileWidth(w: Int, h: Int, columns: Int, n: Int, gap: Int): Float {
        val cols = columns.coerceIn(1, n.coerceAtLeast(1))
        val rows = (n.coerceAtLeast(1) + cols - 1) / cols
        val byWidth = (w - gap * (cols - 1)) / cols.toFloat()
        val byHeight = (h - gap * (rows - 1)) / rows.toFloat() * TILE_ASPECT
        return minOf(byWidth, byHeight).coerceAtLeast(0f)
    }

    /** `selected` with `id` added, or removed when it was there. */
    fun toggle(selected: Set<String>, id: String): Set<String> = if (id in selected) selected - id else selected + id
}

/** Automatic page switching. A manual page change restarts the countdown ([reset]). */
class Rotation(private var since: Long) {
    fun reset(now: Long) {
        since = now
    }

    /** True (and restarts the countdown) when the page should advance now. */
    fun due(intervalMs: Long?, pages: Int, now: Long): Boolean {
        if (intervalMs == null || pages <= 1 || now - since < intervalMs) return false
        since = now
        return true
    }
}

/** A monitor-wall thumbnail from the relay: `[0x11][9 ASCII digits: host clean ID][JPEG]`. */
class Thumbnail(val hostClean: String, val bytes: ByteArray, val offset: Int) {
    val length: Int get() = bytes.size - offset

    companion object {
        const val TAG: Byte = 0x11
        private const val ID_LEN = 9
        private const val HEADER = 1 + ID_LEN

        /** Thumbnails larger than this are refused, as by the relay. */
        const val MAX_JPEG = 512 * 1024

        /** Whether `bytes` carries the monitor tag (sealed and plain session traffic never starts with it). */
        fun isTagged(bytes: ByteArray): Boolean = bytes.isNotEmpty() && bytes[0] == TAG

        /** The thumbnail in `bytes`, or null when the ID is not 9 digits or the payload is not a plain JPEG. */
        fun parse(bytes: ByteArray): Thumbnail? {
            if (!isTagged(bytes) || bytes.size < HEADER + 3 || bytes.size - HEADER > MAX_JPEG) return null
            for (i in 1..ID_LEN) if (bytes[i] !in '0'.code.toByte()..'9'.code.toByte()) return null
            if (bytes[HEADER] != 0xFF.toByte() || bytes[HEADER + 1] != 0xD8.toByte() || bytes[HEADER + 2] != 0xFF.toByte()) return null
            return Thumbnail(String(bytes, 1, ID_LEN, Charsets.US_ASCII), bytes, HEADER)
        }
    }
}
