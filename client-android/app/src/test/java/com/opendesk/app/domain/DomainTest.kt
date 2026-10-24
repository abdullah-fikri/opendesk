package com.opendesk.app.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DomainTest {

    @Test
    fun deskIdsAreCleanedAndFormatted() {
        assertEquals("123456789", DeskId.clean("123 456-789"))
        assertEquals("123 456 789", DeskId.format("123456789"))
        assertEquals("12", DeskId.format("12"))
    }

    @Test
    fun deskIdFromDeviceIdentifierIsStableAndWellFormed() {
        val id = DeskId.fromDeviceIdentifier("abc123")
        assertEquals(id, DeskId.fromDeviceIdentifier("abc123"))
        val digits = DeskId.clean(id)
        assertEquals(9, digits.length)
        assertTrue(digits[0] != '0')
    }

    @Test
    fun gatewayUrlsDefaultToTlsExceptOnLoopback() {
        assertEquals("wss://relay.example.com", GatewayUrl.normalize(" https://relay.example.com/ "))
        assertEquals("wss://relay.example.com", GatewayUrl.normalize("relay.example.com"))
        // Release builds refuse cleartext to LAN addresses, so they default to TLS too.
        assertEquals("wss://192.168.1.5:8088", GatewayUrl.normalize("192.168.1.5:8088"))
        assertEquals("ws://127.0.0.1:8088", GatewayUrl.normalize("127.0.0.1:8088"))
        assertEquals("ws://localhost:8088", GatewayUrl.normalize("localhost:8088"))
        assertEquals("ws://192.168.1.5:8088", GatewayUrl.normalize("ws://192.168.1.5:8088"))
        assertEquals("", GatewayUrl.normalize("  "))
    }

    @Test
    fun gatewayUrlHostAndScheme() {
        assertTrue(GatewayUrl.isPlaintext("ws://localhost:8088"))
        assertFalse(GatewayUrl.isPlaintext("wss://relay.example.com"))
        assertEquals("relay.example.com", GatewayUrl.host("wss://relay.example.com:443/ws"))
        assertEquals("10.0.0.2", GatewayUrl.host("ws://user@10.0.0.2:8088"))
        assertEquals("[::1]", GatewayUrl.host("ws://[::1]:8088"))
    }

    @Test
    fun registersAsViewerOnlyWithoutVideoCapabilities() {
        assertEquals(listOf("viewer-only"), Capabilities.advertised)
        // The relay accepts lowercase letters, digits, '-' and '.', at most 32 characters.
        Capabilities.advertised.forEach { assertTrue(it.matches(Regex("[a-z0-9.-]{1,32}"))) }
    }

    @Test
    fun viewportFitsAndMapsTouches() {
        val v = Viewport()
        // A 1920x1080 frame in a 1000x1000 view: scale 0.5208, letterboxed vertically.
        v.resize(1000, 1000, 1920, 1080)
        assertEquals(1000f, v.drawnWidth, 0.5f)
        assertEquals(562.5f, v.drawnHeight, 0.5f)
        assertEquals(218.75f, v.top, 0.5f)
        assertEquals(0.5f, v.toNormX(500f), 0.001f)
        assertEquals(0.5f, v.toNormY(500f), 0.001f)
        // Touches on the black bars clamp to the frame's edge.
        assertEquals(0f, v.toNormY(10f), 0.001f)
        assertEquals(1f, v.toNormY(990f), 0.001f)
    }

    @Test
    fun viewportZoomKeepsTheFocusPointFixed() {
        val v = Viewport()
        v.resize(1000, 1000, 1000, 1000)
        val before = v.toNormX(250f) to v.toNormY(750f)
        v.zoomBy(2f, 250f, 750f)
        assertEquals(2f, v.zoom, 0.001f)
        assertEquals(before.first, v.toNormX(250f), 0.001f)
        assertEquals(before.second, v.toNormY(750f), 0.001f)
        // Round trip between view and remote coordinates.
        assertEquals(400f, v.toViewX(v.toNormX(400f)), 0.01f)
    }

    @Test
    fun viewportClampsZoomAndPan() {
        val v = Viewport()
        v.resize(1000, 1000, 1000, 1000)
        v.zoomBy(0.5f, 500f, 500f)
        assertEquals(1f, v.zoom, 0.001f)
        v.zoomBy(100f, 500f, 500f)
        assertEquals(Viewport.MAX_ZOOM, v.zoom, 0.001f)
        // Panning far right stops with the frame's left edge at the view's left edge.
        v.panBy(1_000_000f, 0f)
        assertEquals(0f, v.left, 0.01f)
        v.reset()
        assertEquals(1f, v.zoom, 0.001f)
        assertEquals(0f, v.left, 0.01f)
    }

    @Test
    fun viewportFollowsTheCursorWhenZoomed() {
        val v = Viewport()
        v.resize(1000, 1000, 1000, 1000)
        v.zoomBy(4f, 500f, 500f)
        v.ensureVisible(0.95f, 0.05f, 48f)
        assertTrue(v.toViewX(0.95f) <= 1000f - 48f + 0.01f)
        assertTrue(v.toViewY(0.05f) >= 48f - 0.01f)
    }

    @Test
    fun androidKeysMapToWin32VirtualKeys() {
        assertEquals(WindowsKeys.VK_BACK, WindowsKeys.fromAndroid(67))
        assertEquals(WindowsKeys.VK_RETURN, WindowsKeys.fromAndroid(66))
        assertEquals(WindowsKeys.VK_LEFT, WindowsKeys.fromAndroid(21))
        assertNull(WindowsKeys.fromAndroid(29)) // KEYCODE_A is printable: sent as text
    }

    @Test
    fun composingTextIsMirroredByDiff() {
        assertEquals(0 to "lo", WindowsKeys.diff("hel", "hello"))
        assertEquals(2 to "p", WindowsKeys.diff("hello", "help"))
        assertEquals(3 to "", WindowsKeys.diff("abc", ""))
        // An emoji is one Backspace on the host, not two.
        assertEquals(1 to "", WindowsKeys.diff("a😀", "a"))
    }
}
