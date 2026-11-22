package com.opendesk.app.ui.viewer.keyboard

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class GlassBlurTest {
    @Test
    fun blurAveragesAndKeepsFlatImagesFlat() {
        val flat = IntArray(20 * 10) { 0xFF336699.toInt() }
        GlassBlur.blur(flat, 20, 10, 3)
        assertTrue(flat.all { it == 0xFF336699.toInt() })
        // A single white pixel spreads out and loses intensity.
        val dot = IntArray(9 * 9) { 0xFF000000.toInt() }
        dot[4 * 9 + 4] = 0xFFFFFFFF.toInt()
        GlassBlur.blur(dot, 9, 9, 1)
        val center = dot[4 * 9 + 4] and 0xFF
        val neighbor = dot[4 * 9 + 5] and 0xFF
        assertTrue("center $center", center in 1..254)
        assertTrue("neighbor $neighbor", neighbor in 1..center)
        assertEquals(0xFF, dot[0] ushr 24)
    }
}
