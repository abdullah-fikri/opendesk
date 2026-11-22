package com.opendesk.app.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class FocusPanTest {

    /** A 1000 x 2000 portrait view showing a 1920 x 1080 screen (fit: 1000 x 562.5, centered). */
    private fun portrait() = Viewport().apply { resize(1000, 2000, 1920, 1080) }

    private fun Viewport.apply(s: FocusPan.State) = set(s.zoom, s.panX, s.panY)

    private val margin = 24f
    private val minHeight = 20f

    @Test
    fun fieldUnderTheKeyboardMovesAboveIt() {
        val v = Viewport().apply { resize(1000, 1000, 1000, 1000) }
        v.zoomBy(2f, 500f, 500f) // 2000 x 2000 drawn, pan limits +-500
        // A field near the bottom of the frame, 100 px high once zoomed.
        val field = FocusPan.Field(0.4f, 0.8f, 0.2f, 0.05f)
        val keyboardTop = 600f
        val t = FocusPan.target(v, field, keyboardTop, margin, minHeight)
        v.bottomInset = FocusPan.inset(1000f, keyboardTop, margin)
        v.apply(t)
        assertEquals(keyboardTop - margin, v.toViewY(field.y + field.h), 0.5f)
        assertEquals(2f, v.zoom, 0.001f)
    }

    @Test
    fun aFittedFrameCanRiseAboveAFloatingKeyboard() {
        val v = portrait()
        // Frame spans y 718.75 .. 1281.25; a field at its bottom; keyboard top at 1100.
        val field = FocusPan.Field(0.1f, 0.9f, 0.3f, 0.06f)
        val t = FocusPan.target(v, field, 1100f, margin, minHeight)
        assertTrue("moved up: ${t.panY}", t.panY < 0f)
        v.bottomInset = FocusPan.inset(2000f, 1100f, margin)
        v.apply(t)
        assertEquals(1100f - margin, v.toViewY(field.y + field.h), 0.5f)
        // Without the keyboard's inset the same pan is out of limits.
        val plain = portrait()
        plain.set(t.zoom, t.panX, t.panY)
        assertTrue(plain.panY > t.panY)
    }

    @Test
    fun aVisibleFieldStaysPut() {
        val v = portrait()
        val field = FocusPan.Field(0.2f, 0.1f, 0.3f, 0.06f) // near the frame's top, well above the keyboard
        val t = FocusPan.target(v, field, 1400f, margin, minHeight)
        assertTrue(t.near(FocusPan.state(v)))
    }

    @Test
    fun aTinyFieldIsZoomedIn() {
        val v = portrait() // fit scale 0.52: a 20 px high field shows ~10 px
        val field = FocusPan.Field(0.45f, 0.5f, 0.1f, 20f / 1080f)
        val t = FocusPan.target(v, field, 1200f, margin, minHeight)
        assertTrue("zoom ${t.zoom}", t.zoom >= FocusPan.MIN_ZOOM)
        assertTrue("zoom ${t.zoom}", t.zoom <= FocusPan.MAX_AUTO_ZOOM)
        v.bottomInset = FocusPan.inset(2000f, 1200f, margin)
        v.apply(t)
        val shown = v.toViewY(field.y + field.h) - v.toViewY(field.y)
        assertTrue("shown $shown", shown >= minHeight - 0.5f)
        assertTrue(v.toViewY(field.y + field.h) <= 1200f - margin + 0.5f)
        assertTrue(v.toViewY(field.y) >= 0f)
        assertTrue(v.toViewX(field.x) >= 0f && v.toViewX(field.x + field.w) <= 1000f)
    }

    @Test
    fun zoomNeverGoesPastTheCap() {
        val v = portrait()
        val field = FocusPan.Field(0.5f, 0.5f, 0.01f, 0.0005f)
        val t = FocusPan.target(v, field, 1200f, margin, minHeight)
        assertEquals(FocusPan.MAX_AUTO_ZOOM, t.zoom, 0.001f)
    }

    @Test
    fun aFieldAboveTheViewComesDown() {
        val v = Viewport().apply { resize(1000, 1000, 1000, 1000) }
        v.zoomBy(3f, 500f, 1000f) // looking at the bottom of the frame
        val field = FocusPan.Field(0.4f, 0.05f, 0.2f, 0.03f)
        val t = FocusPan.target(v, field, 600f, margin, minHeight)
        v.bottomInset = FocusPan.inset(1000f, 600f, margin)
        v.apply(t)
        assertEquals(margin, v.toViewY(field.y), 0.5f)
    }

    @Test
    fun aWideFieldShowsItsStart() {
        val v = Viewport().apply { resize(1000, 1000, 1000, 1000) }
        v.zoomBy(3f, 1000f, 500f) // looking at the right edge
        val field = FocusPan.Field(0.05f, 0.5f, 0.9f, 0.03f) // 2700 px wide once zoomed
        val t = FocusPan.target(v, field, 800f, margin, minHeight)
        v.bottomInset = FocusPan.inset(1000f, 800f, margin)
        v.apply(t)
        assertEquals(margin, v.toViewX(field.x), 0.5f)
    }

    @Test
    fun aHugeFieldPartlyInViewDoesNotMove() {
        val v = Viewport().apply { resize(1000, 1000, 1000, 1000) }
        // A document filling the frame: taller than the room above the keyboard, but in view.
        val field = FocusPan.Field(0f, 0f, 1f, 1f)
        val t = FocusPan.target(v, field, 600f, margin, minHeight)
        assertTrue(t.near(FocusPan.state(v)))
    }

    @Test
    fun lerpGoesFromStartToEnd() {
        val a = FocusPan.State(1f, 0f, 0f)
        val b = FocusPan.State(2f, -100f, 50f)
        assertEquals(a, FocusPan.lerp(a, b, 0f))
        assertEquals(b, FocusPan.lerp(a, b, 1f))
        assertEquals(FocusPan.State(1.5f, -50f, 25f), FocusPan.lerp(a, b, 0.5f))
    }

    @Test
    fun fieldsFromEvents() {
        assertNull(FocusPan.Field.of(null, 0.1f, 0.1f, 0.1f))
        assertNull(FocusPan.Field.of(0.1f, 0.1f, 0f, 0.1f))
        assertNull(FocusPan.Field.of(Float.NaN, 0.1f, 0.1f, 0.1f))
        assertNotNull(FocusPan.Field.of(0.1f, 0.2f, 0.3f, 0.04f))
    }

    @Test
    fun viewportInsetOnlyAddsRoomBelow() {
        val v = portrait()
        v.panBy(0f, -500f)
        assertEquals(0f, v.panY, 0.001f) // fitted frame: no room
        v.bottomInset = 900f
        v.panBy(0f, -500f)
        // Frame bottom may reach the keyboard top (1100): bottom 1281.25 - 181.25.
        assertEquals(-181.25f, v.panY, 0.01f)
        v.bottomInset = 0f
        assertEquals(0f, v.panY, 0.001f)
        val copy = v.copy()
        assertEquals(v.zoom, copy.zoom, 0f)
        assertEquals(v.left, copy.left, 0f)
    }
}
