package com.opendesk.app.ui.viewer.keyboard

import com.opendesk.app.domain.WindowsKeys

/**
 * The glass keyboard's layout (pure, unit-tested): key rows of each page, the split halves, their geometry, hit
 * testing and the keyboard's sizes. What a key press does is [KeyInput]; when the keyboard shows itself is
 * [AutoKeyboard].
 */
object KeyboardLayouts {

    enum class Page { LETTERS, NUMBERS, SYMBOLS }

    enum class Kind {
        /** Types [Key.text] (upper case while Shift is on). */
        CHAR,
        SHIFT,
        BACKSPACE,
        ENTER,
        SPACE,

        /** A non-printing key ([Key.vk]): Esc, Tab, arrows. */
        SPECIAL,

        /** A sticky modifier ([Key.mod]). */
        MOD,

        /** Switches to [Key.page]. */
        PAGE,
    }

    enum class Mod { CTRL, ALT, WIN }

    data class Key(
        val kind: Kind,
        val label: String,
        /** Relative width in its row (a letter key is 1). */
        val weight: Float = 1f,
        val text: String = "",
        val vk: Int = 0,
        val mod: Mod? = null,
        val page: Page? = null,
    ) {
        /** Function keys get the fainter fill. */
        val isFunction: Boolean get() = kind != Kind.CHAR && kind != Kind.SPACE
    }

    private fun chars(s: String) = s.map { Key(Kind.CHAR, it.toString(), text = it.toString()) }
    private fun special(label: String, vk: Int, weight: Float = 1f) = Key(Kind.SPECIAL, label, weight, vk = vk)
    private fun mod(label: String, mod: Mod) = Key(Kind.MOD, label, 1.1f, mod = mod)

    /** Esc · Tab · Ctrl · Alt · Win · ← ↑ ↓ →, above every page. */
    val modifierRow: List<Key> = listOf(
        special("Esc", WindowsKeys.VK_ESCAPE, 1.1f),
        special("Tab", WindowsKeys.VK_TAB, 1.1f),
        mod("Ctrl", Mod.CTRL),
        mod("Alt", Mod.ALT),
        mod("Win", Mod.WIN),
        special("←", WindowsKeys.VK_LEFT),
        special("↑", WindowsKeys.VK_UP),
        special("↓", WindowsKeys.VK_DOWN),
        special("→", WindowsKeys.VK_RIGHT),
    )

    private val backspace = Key(Kind.BACKSPACE, "⌫", 1.5f)
    private val enter = Key(Kind.ENTER, "⏎", 1.8f)
    private val space = Key(Kind.SPACE, "", 4.6f, text = " ")
    private fun pageKey(label: String, page: Page, weight: Float = 1.5f) = Key(Kind.PAGE, label, weight, page = page)

    /** Rows of `page`, the modifier row first. */
    fun rows(page: Page): List<List<Key>> = listOf(modifierRow) + when (page) {
        Page.LETTERS -> listOf(
            chars("qwertyuiop"),
            chars("asdfghjkl"),
            listOf(Key(Kind.SHIFT, "⇧", 1.5f)) + chars("zxcvbnm") + backspace,
            listOf(pageKey("123", Page.NUMBERS), chars(",")[0], space, chars(".")[0], enter),
        )
        Page.NUMBERS -> listOf(
            chars("1234567890"),
            chars("-/:;()$&@\""),
            listOf(pageKey("#+=", Page.SYMBOLS)) + chars(".,?!'") + backspace,
            listOf(pageKey("ABC", Page.LETTERS), chars(",")[0], space, chars(".")[0], enter),
        )
        Page.SYMBOLS -> listOf(
            chars("[]{}#%^*+="),
            chars("_\\|~<>€£¥•"),
            listOf(pageKey("123", Page.NUMBERS)) + chars(".,?!'") + backspace,
            listOf(pageKey("ABC", Page.LETTERS), chars(",")[0], space, chars(".")[0], enter),
        )
    }

    private val splitSpace = Key(Kind.SPACE, "", 3f, text = " ")

    /**
     * The landscape split keyboard (like a game pad's thumbs): `page` cut into a left and a right half, each with
     * its part of the modifier row first (Esc · Tab · Ctrl · Alt | Win · arrows) and a Space of its own. Letters split
     * like hardware split keyboards (q-t, a-g, z-b | y-p, h-l, n-m); the other pages split each row in the middle.
     */
    fun splitRows(page: Page): Pair<List<List<Key>>, List<List<Key>>> {
        val full = rows(page)
        val bottom = full.last()
        val spaceAt = bottom.indexOfFirst { it.kind == Kind.SPACE }
        val bottomLeft = bottom.take(spaceAt) + splitSpace
        val bottomRight = listOf(splitSpace) + bottom.drop(spaceAt + 1)
        val (left, right) = if (page == Page.LETTERS) {
            val row3 = full[3]
            listOf(chars("qwert"), chars("asdfg"), row3.take(6)) to
                listOf(chars("yuiop"), chars("hjkl"), row3.drop(6))
        } else {
            val middle = full.subList(1, full.size - 1)
            middle.map { it.take((it.size + 1) / 2) } to middle.map { it.drop((it.size + 1) / 2) }
        }
        return (listOf(modifierRow.take(SPLIT_MODS_LEFT)) + left + listOf(bottomLeft)) to
            (listOf(modifierRow.drop(SPLIT_MODS_LEFT)) + right + listOf(bottomRight))
    }

    private const val SPLIT_MODS_LEFT = 4

    /**
     * Places the split halves of [splitRows]: the left one in `[left, left + halfWidth]`, the right one in
     * `[left + width - halfWidth, left + width]`, both from `top` down `height`, keys the same size in both halves.
     * Right-half keys have [Placed.side] 1.
     */
    fun placeSplit(
        halves: Pair<List<List<Key>>, List<List<Key>>>,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        halfWidth: Float,
        gap: Float,
    ): List<Placed> {
        val half = halfWidth.coerceAtMost(width / 2)
        if (half <= 0f || height <= 0f) return emptyList()
        val unit = (halves.first.drop(1) + halves.second.drop(1)).minOfOrNull { unitFor(it, half, gap) } ?: return emptyList()
        return place(halves.first, left, top, half, height, gap, unit, side = 0) +
            place(halves.second, left + width - half, top, half, height, gap, unit, side = 1)
    }

    /** A key placed on the keyboard, in pixels; `side` 1 is the right half of the split keyboard. */
    data class Placed(
        val key: Key,
        val row: Int,
        val left: Float,
        val top: Float,
        val right: Float,
        val bottom: Float,
        val side: Int = 0,
    ) {
        val centerX: Float get() = (left + right) / 2
        val width: Float get() = right - left
        val height: Float get() = bottom - top
    }

    /**
     * Places `rows` in the box `[left, top] .. [left + width, top + height]`: equal row heights separated by `gap`,
     * each row's keys sharing the width by weight. Rows lighter than the widest are centered at the same key scale
     * (like the shorter second row of a phone keyboard). `keyUnit` overrides that scale (one letter key's width).
     */
    fun place(
        rows: List<List<Key>>,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        gap: Float,
        keyUnit: Float? = null,
        side: Int = 0,
    ): List<Placed> {
        if (rows.isEmpty() || width <= 0f || height <= 0f) return emptyList()
        val rowHeight = (height - gap * (rows.size - 1)) / rows.size
        // The first (modifier) row spans the whole width; the others share the scale of the fullest one.
        val unit = keyUnit ?: rows.drop(1).minOfOrNull { unitFor(it, width, gap) } ?: unitFor(rows[0], width, gap)
        val out = ArrayList<Placed>()
        rows.forEachIndexed { r, row ->
            val rowUnit = if (r == 0) unitFor(row, width, gap) else unit
            val used = row.sumOf { it.weight.toDouble() }.toFloat() * rowUnit + gap * (row.size - 1)
            var x = left + (width - used) / 2
            val y = top + r * (rowHeight + gap)
            row.forEach { key ->
                val w = key.weight * rowUnit
                out += Placed(key, r, x, y, x + w, y + rowHeight, side)
                x += w + gap
            }
        }
        return out
    }

    private fun unitFor(row: List<Key>, width: Float, gap: Float): Float =
        (width - gap * (row.size - 1)) / row.sumOf { it.weight.toDouble() }.toFloat()

    /**
     * The key under `(x, y)`: the touched row (gaps belong to the nearer row), then the key whose span is nearest,
     * so a touch between two keys or at a row's padded end still hits one. Null outside the rows' vertical span.
     */
    fun hit(keys: List<Placed>, x: Float, y: Float, slop: Float = 0f): Placed? {
        if (keys.isEmpty()) return null
        val top = keys.minOf { it.top } - slop
        val bottom = keys.maxOf { it.bottom } + slop
        if (y < top || y > bottom) return null
        val row = keys.minByOrNull { distance(y, it.top, it.bottom) }!!.row
        return keys.filter { it.row == row }.minByOrNull { distance(x, it.left, it.right) }
    }

    private fun distance(v: Float, from: Float, to: Float): Float = when {
        v < from -> from - v
        v > to -> v - to
        else -> 0f
    }

    /** The keyboard's scale (fraction of the available width) stays within these. */
    const val MIN_SCALE = 0.6f
    const val MAX_SCALE = 1f

    fun clampScale(scale: Float): Float = scale.coerceIn(MIN_SCALE, MAX_SCALE)

    /**
     * Default size as fractions of the screen: portrait full width at 36 % of the height; landscape a compact
     * keyboard (62 % wide, 46 % high) that leaves the remote screen visible around it.
     */
    fun defaultSize(landscape: Boolean): Pair<Float, Float> = if (landscape) 0.62f to 0.46f else 1f to 0.36f

    /** Split keyboard: each half 30 % of the width and 50 % of the height (the middle 40 % stays free). */
    fun splitSize(): Pair<Float, Float> = 0.3f to 0.5f

    /** Landscape opens split on phones; a tablet has room for the whole keyboard. */
    fun defaultSplit(tablet: Boolean): Boolean = !tablet

}
