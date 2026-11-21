package com.opendesk.app.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test

class RenderPipelineTest {
    private class Buf(val size: Int)

    private fun SwapChain<Buf>.take(size: Int = 1, dropped: MutableList<Buf> = mutableListOf()) =
        take({ it.size == size }, { Buf(size) }, { dropped.add(it) })

    @Test
    fun swapChainNeverHandsOutWhatTheScreenMayStillDraw() {
        val chain = SwapChain<Buf>()
        val shown = ArrayDeque<Buf>()
        repeat(20) {
            // Nothing waits for the main thread, so a buffer is always available.
            val b = chain.take()
            assertNotNull(b)
            // Never the one on screen nor the one it just replaced.
            shown.takeLast(2).forEach { assertNotSame(it, b) }
            chain.shown(b!!)
            shown.addLast(b)
        }
        assertEquals(3, chain.size)
        assertEquals(1, chain.freeCount)
    }

    @Test
    fun swapChainIsBoundedAndReplacesBuffersOfAnotherSize() {
        val chain = SwapChain<Buf>()
        val a = chain.take()!!
        val b = chain.take()!!
        val c = chain.take()!!
        assertNull("all three are taken", chain.take())
        chain.giveBack(c)
        assertSame(c, chain.take())
        chain.shown(a)
        chain.shown(b)
        chain.shown(c)
        // `a` is free again; a new frame size drops it and creates a buffer of that size instead.
        val dropped = mutableListOf<Buf>()
        val big = chain.take(size = 2, dropped = dropped)
        assertEquals(listOf(a), dropped)
        assertEquals(2, big!!.size)
        assertEquals(3, chain.size)
        // Showing the same buffer twice frees nothing.
        chain.shown(c)
        assertEquals(0, chain.freeCount)
    }

    @Test
    fun streamBoxIsTheDisplayInLandscapeCappedAt1080Rows() {
        assertEquals(2400 to 1080, StreamRequest.box(1080, 2400))
        assertEquals(2400 to 1080, StreamRequest.box(2400, 1080))
        assertEquals(1600 to 720, StreamRequest.box(720, 1600))
        // 1440p phones ask for no more than the host's default preset streams.
        assertEquals(2400 to 1080, StreamRequest.box(1440, 3200))
        assertEquals(1350 to 1080, StreamRequest.box(1800, 2250))
        assertEquals(1998 to 1080, StreamRequest.box(1081, 2001))
        assertNull(StreamRequest.box(0, 2400))
        assertNull(StreamRequest.box(100, 100))
    }
}
