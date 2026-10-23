package com.opendesk.app.domain

import org.junit.Assert.assertEquals
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
    fun gatewayUrlsMatchTheDesktopRules() {
        assertEquals("wss://relay.example.com", GatewayUrl.normalize(" https://relay.example.com/ "))
        assertEquals("ws://192.168.1.5:8088", GatewayUrl.normalize("192.168.1.5:8088"))
        assertEquals("wss://relay.example.com", GatewayUrl.normalize("relay.example.com"))
        assertEquals("", GatewayUrl.normalize("  "))
    }
}
