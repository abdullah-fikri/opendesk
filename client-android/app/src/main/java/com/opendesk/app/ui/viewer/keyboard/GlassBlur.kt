package com.opendesk.app.ui.viewer.keyboard

/** The glass backdrop's blur: a box blur run twice (close to a Gaussian) on a small ARGB image, in place. */
object GlassBlur {
    fun blur(pixels: IntArray, width: Int, height: Int, radius: Int) {
        if (radius < 1 || width < 1 || height < 1 || pixels.size < width * height) return
        val tmp = IntArray(width * height)
        repeat(2) {
            pass(pixels, tmp, width, height, radius, horizontal = true)
            pass(tmp, pixels, width, height, radius, horizontal = false)
        }
    }

    private fun pass(src: IntArray, dst: IntArray, w: Int, h: Int, r: Int, horizontal: Boolean) {
        val lines = if (horizontal) h else w
        val len = if (horizontal) w else h
        for (line in 0 until lines) {
            val index = { i: Int -> if (horizontal) line * w + i else i * w + line }
            var a = 0; var red = 0; var g = 0; var b = 0
            // Window [i - r, i + r], edges clamped.
            for (k in -r..r) {
                val c = src[index(k.coerceIn(0, len - 1))]
                a += c ushr 24; red += (c shr 16) and 0xFF; g += (c shr 8) and 0xFF; b += c and 0xFF
            }
            val n = 2 * r + 1
            for (i in 0 until len) {
                dst[index(i)] = ((a / n) shl 24) or ((red / n) shl 16) or ((g / n) shl 8) or (b / n)
                val out = src[index((i - r).coerceIn(0, len - 1))]
                val inc = src[index((i + r + 1).coerceIn(0, len - 1))]
                a += (inc ushr 24) - (out ushr 24)
                red += ((inc shr 16) and 0xFF) - ((out shr 16) and 0xFF)
                g += ((inc shr 8) and 0xFF) - ((out shr 8) and 0xFF)
                b += (inc and 0xFF) - (out and 0xFF)
            }
        }
    }
}
