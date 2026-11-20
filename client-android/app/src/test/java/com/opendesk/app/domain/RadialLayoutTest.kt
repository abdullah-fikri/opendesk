package com.opendesk.app.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class RadialLayoutTest {
    private fun menu(count: Int, right: Boolean = false) =
        RadialLayout(count, radius = 100f, itemRadius = 20f, deadZone = 30f, opensRight = right).apply {
            place(900f, 500f, 1000f, 1000f, 10f)
        }

    @Test
    fun opensAwayFromTheEdgeAndStaysOnScreen() {
        val m = menu(4)
        for (i in 0 until 4) {
            assertTrue(m.itemX(i) <= m.centerX + 0.01f)
            assertTrue(m.itemX(i) + 20f <= 1000f)
        }
        // A finger in the corner moves the center inward so the top item is not cut off.
        m.place(995f, 5f, 1000f, 1000f, 10f)
        assertEquals(990f, m.centerX, 0.01f)
        assertTrue(m.itemY(0) - 20f >= 0f)
    }

    @Test
    fun picksTheItemTheFingerPointsAt() {
        val m = menu(4)
        for (i in 0 until 4) assertEquals(i, m.pick(m.itemX(i), m.itemY(i)))
        // Farther out along the same direction still picks it.
        assertEquals(0, m.pick(m.itemX(0, 180f), m.itemY(0, 180f)))
    }

    @Test
    fun deadZoneAndBehindPickNothing() {
        val m = menu(4)
        assertEquals(-1, m.pick(m.centerX + 5f, m.centerY))
        assertEquals(-1, m.pick(m.centerX + 80f, m.centerY))
    }

    @Test
    fun leftEdgeMenuMirrors() {
        val m = RadialLayout(3, 100f, 20f, 30f, opensRight = true).apply { place(10f, 500f, 1000f, 1000f, 10f) }
        for (i in 0 until 3) {
            assertTrue(m.itemX(i) >= m.centerX - 0.01f)
            assertEquals(i, m.pick(m.itemX(i), m.itemY(i)))
        }
    }
}
