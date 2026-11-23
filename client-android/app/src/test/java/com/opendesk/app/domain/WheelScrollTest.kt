package com.opendesk.app.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class WheelScrollTest {
    @Test
    fun smallMovesAddUpToWholeLines() {
        val s = WheelScroll(unitsPerRemotePx = 1f)
        assertEquals(0, s.add(30f))
        assertEquals(40, s.add(30f)) // 60 units: one line, 20 kept
        assertEquals(80, s.add(70f)) // 90 units
        assertEquals(0, s.add(5f))
    }

    @Test
    fun turningAroundDropsTheOtherDirectionsRest() {
        val s = WheelScroll(unitsPerRemotePx = 1f)
        assertEquals(0, s.add(39f))
        assertEquals(0, s.add(-39f))
        assertEquals(-40, s.add(-1f))
    }

    @Test
    fun neverSendsLessThanALine() {
        val s = WheelScroll()
        for (i in 0 until 100) {
            val d = s.add(0.7f)
            assertTrue(d == 0 || d % WheelScroll.STEP == 0)
        }
    }

    @Test
    fun flickDecaysAndStops() {
        var v = 3000f
        var frames = 0
        while (WheelScroll.coasting(v)) {
            v = WheelScroll.decay(v, 16f)
            frames++
        }
        assertTrue(frames in 20..60)
        assertFalse(WheelScroll.coasting(200f))
    }
}
