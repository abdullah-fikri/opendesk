package com.opendesk.app.ui.viewer.keyboard

import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Kind
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Mod
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Page
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class KeyboardLayoutsTest {


    private fun key(page: Page, label: String) = KeyboardLayouts.rows(page).flatten().first { it.label == label }

    @Test
    fun everyPageHasTheModifierRowAndTheBasics() {
        for (page in Page.values()) {
            val rows = KeyboardLayouts.rows(page)
            assertEquals(KeyboardLayouts.modifierRow, rows[0])
            val kinds = rows.flatten().map { it.kind }.toSet()
            assertTrue(page.name, kinds.containsAll(listOf(Kind.BACKSPACE, Kind.ENTER, Kind.SPACE, Kind.PAGE)))
        }
        assertEquals("qwertyuiop", KeyboardLayouts.rows(Page.LETTERS)[1].joinToString("") { it.text })
        assertEquals("1234567890", KeyboardLayouts.rows(Page.NUMBERS)[1].joinToString("") { it.text })
        assertTrue(KeyboardLayouts.rows(Page.LETTERS).flatten().any { it.kind == Kind.SHIFT })
    }

    @Test
    fun keysFillTheBoxWithoutOverlapping() {
        val keys = KeyboardLayouts.place(KeyboardLayouts.rows(Page.LETTERS), 10f, 20f, 1000f, 400f, 6f)
        assertEquals(KeyboardLayouts.rows(Page.LETTERS).flatten().size, keys.size)
        keys.forEach { k ->
            assertTrue(k.toString(), k.left >= 10f - 0.01f && k.right <= 1010f + 0.01f)
            assertTrue(k.toString(), k.top >= 20f - 0.01f && k.bottom <= 420f + 0.01f)
        }
        keys.groupBy { it.row }.values.forEach { row ->
            row.zipWithNext().forEach { (a, b) -> assertTrue(a.right <= b.left + 0.01f) }
        }
        // The modifier row and the top letter row both span the whole width.
        assertEquals(10f, keys.first { it.row == 0 }.left, 0.01f)
        assertEquals(1010f, keys.last { it.row == 1 }.right, 0.01f)
        // The shorter middle row is centered at the same key size.
        val q = keys.first { it.key.text == "q" }
        val a = keys.first { it.key.text == "a" }
        assertEquals(q.width, a.width, 0.01f)
        assertTrue(a.left > q.left)
    }

    @Test
    fun hitsTheNearestKeyEvenInGaps() {
        val keys = KeyboardLayouts.place(KeyboardLayouts.rows(Page.LETTERS), 0f, 0f, 1000f, 500f, 10f)
        val q = keys.first { it.key.text == "q" }
        val w = keys.first { it.key.text == "w" }
        assertEquals("q", KeyboardLayouts.hit(keys, q.centerX, (q.top + q.bottom) / 2)?.key?.text)
        // Just right of q, in the gap: still q.
        assertEquals("q", KeyboardLayouts.hit(keys, q.right + 2f, q.top + 5f)?.key?.text)
        assertEquals("w", KeyboardLayouts.hit(keys, w.left - 2f, w.top + 5f)?.key?.text)
        // Left of the centered "a" row: a.
        val a = keys.first { it.key.text == "a" }
        assertEquals("a", KeyboardLayouts.hit(keys, 0f, (a.top + a.bottom) / 2)?.key?.text)
        assertNull(KeyboardLayouts.hit(keys, 10f, 600f))
        assertNotNull(KeyboardLayouts.hit(keys, 10f, 505f, slop = 10f))
    }

    @Test
    fun scaleStaysBetweenSixtyAndHundredPercent() {
        assertEquals(0.6f, KeyboardLayouts.clampScale(0.1f), 0f)
        assertEquals(1f, KeyboardLayouts.clampScale(3f), 0f)
        assertEquals(0.8f, KeyboardLayouts.clampScale(0.8f), 0f)
        assertEquals(1f to 0.36f, KeyboardLayouts.defaultSize(landscape = false))
        assertTrue(KeyboardLayouts.defaultSize(landscape = true).first < 1f)
    }

    private fun texts(rows: List<List<KeyboardLayouts.Key>>) = rows.drop(1).flatten().filter { it.kind == Kind.CHAR }.joinToString("") { it.text }

    @Test
    fun splitLettersLikeASplitKeyboard() {
        val (left, right) = KeyboardLayouts.splitRows(Page.LETTERS)
        assertEquals(listOf("qwert", "asdfg", "zxcvb"), left.subList(1, 4).map { row -> row.filter { it.kind == Kind.CHAR }.joinToString("") { it.text } })
        assertEquals(listOf("yuiop", "hjkl", "nm"), right.subList(1, 4).map { row -> row.filter { it.kind == Kind.CHAR }.joinToString("") { it.text } })
        assertEquals(Kind.SHIFT, left[3].first().kind)
        assertEquals(Kind.BACKSPACE, right[3].last().kind)
        // The modifier row is split too, nothing lost or doubled.
        assertEquals(KeyboardLayouts.modifierRow, left[0] + right[0])
        assertTrue(left[0].any { it.mod == Mod.CTRL })
        // Space on both halves, Enter on the right, the page key on the left.
        assertEquals(Kind.SPACE, left.last().last().kind)
        assertEquals(Kind.SPACE, right.last().first().kind)
        assertEquals(Kind.ENTER, right.last().last().kind)
        assertEquals(Kind.PAGE, left.last().first().kind)
    }

    @Test
    fun everySplitPageKeepsEveryKey() {
        for (page in Page.values()) {
            val (left, right) = KeyboardLayouts.splitRows(page)
            val whole = KeyboardLayouts.rows(page)
            assertEquals(page.name, texts(whole).toSortedSet(), (texts(left) + texts(right)).toSortedSet())
            assertEquals(page.name, whole.size, left.size)
            assertEquals(page.name, whole.size, right.size)
            for (kind in listOf(Kind.BACKSPACE, Kind.ENTER, Kind.PAGE)) {
                assertEquals("$page $kind", whole.flatten().count { it.kind == kind }, (left + right).flatten().count { it.kind == kind })
            }
            assertEquals(page.name, 2, (left + right).flatten().count { it.kind == Kind.SPACE })
        }
    }

    @Test
    fun splitHalvesSitInTheCornersWithTheMiddleFree() {
        val halves = KeyboardLayouts.splitRows(Page.LETTERS)
        val keys = KeyboardLayouts.placeSplit(halves, 0f, 100f, 2000f, 400f, 600f, 6f)
        val (left, right) = keys.partition { it.side == 0 }
        assertEquals(halves.first.flatten().size, left.size)
        assertEquals(halves.second.flatten().size, right.size)
        left.forEach { assertTrue(it.toString(), it.left >= -0.01f && it.right <= 600.01f) }
        right.forEach { assertTrue(it.toString(), it.left >= 1399.99f && it.right <= 2000.01f) }
        keys.forEach { assertTrue(it.toString(), it.top >= 99.99f && it.bottom <= 500.01f) }
        // Letter keys are the same size on both halves.
        val widths = keys.filter { it.key.kind == Kind.CHAR }.map { it.width }
        assertEquals(widths.min(), widths.max(), 0.01f)
        // A touch on a half hits that half's key.
        val t = left.first { it.key.text == "t" }
        assertEquals("t", KeyboardLayouts.hit(left, t.centerX, (t.top + t.bottom) / 2)?.key?.text)
        val y = right.first { it.key.text == "y" }
        assertEquals("y", KeyboardLayouts.hit(right, y.centerX, (y.top + y.bottom) / 2)?.key?.text)
    }

    @Test
    fun splitByDefaultOnPhonesOnly() {
        assertTrue(KeyboardLayouts.defaultSplit(tablet = false))
        assertFalse(KeyboardLayouts.defaultSplit(tablet = true))
    }
}
