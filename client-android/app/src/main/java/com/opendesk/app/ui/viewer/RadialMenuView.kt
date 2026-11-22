package com.opendesk.app.ui.viewer

import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.os.SystemClock
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.animation.DecelerateInterpolator
import androidx.core.content.ContextCompat
import androidx.core.content.res.ResourcesCompat
import com.opendesk.app.R
import com.opendesk.app.domain.RadialLayout
import kotlin.math.max
import kotlin.math.min

/**
 * The viewer's radial menu: a half circle of actions that opens around the finger held at the screen edge
 * ([EdgeHandle]). Slide onto an action and lift to run it; lift near the center to cancel. Holding on an action
 * with [Item.children] opens them in place, and holding in the center goes back.
 *
 * Opened with a tap instead of a hold it stays open ("sticky") and takes taps itself. While closed it takes no
 * touches, so the remote screen below gets them.
 */
class RadialMenuView(context: Context) : View(context) {

    /** One action: an icon or a short [glyph] (key names), its [label], and whether it is switched on. */
    class Item(
        val id: Any,
        val label: String,
        val icon: Int = 0,
        val glyph: String? = null,
        val active: Boolean = false,
        val children: List<Item> = emptyList(),
    )

    /** An action without children was chosen. */
    var onPick: ((Item) -> Unit)? = null

    private val density = resources.displayMetrics.density
    private fun dp(v: Float) = v * density

    private val radius = dp(108f)
    private val itemRadius = dp(26f)
    private val deadZone = dp(44f)

    private var root: List<Item> = emptyList()
    private var items: List<Item> = emptyList()
    private var parents = ArrayDeque<List<Item>>()
    private var layout = RadialLayout(1, radius, itemRadius, deadZone)
    private var fingerX = 0f
    private var fingerY = 0f

    val isOpen: Boolean get() = visibility == VISIBLE && !closing
    private var sticky = false
    private var closing = false
    private var highlight = -1

    /** When the finger came to rest on the current spot (an item, or the center), for hold-to-open-children. */
    private var restSince = 0L

    /** 0..1: the half circle and its items flying out. */
    private var progress = 0f

    /** 0..1: the highlighted item growing. */
    private var focus = 0f
    private var animator: ValueAnimator? = null
    private var focusAnimator: ValueAnimator? = null

    private val scrimPaint = Paint().apply { color = SCRIM }
    private val ringPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        color = RING
        strokeCap = Paint.Cap.ROUND
    }
    private val ringEdgePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        color = RING_EDGE
        strokeWidth = max(1f, dp(1f))
    }
    private val itemPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val itemEdgePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = max(1f, dp(1f))
    }
    private val glyphPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.CENTER
        textSize = dp(13f)
        typeface = ResourcesCompat.getFont(context, R.font.inter_semibold)
    }
    private val labelPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textAlign = Paint.Align.CENTER
        textSize = dp(13f)
        color = TEXT
        typeface = ResourcesCompat.getFont(context, R.font.inter_semibold)
    }
    private val labelBg = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = LABEL_BG }
    private val accent = ContextCompat.getColor(context, R.color.accent)
    private val arc = RectF()
    private val pill = RectF()
    private val icons = HashMap<Int, android.graphics.drawable.Drawable>()

    private val holdCheck = object : Runnable {
        override fun run() {
            if (!isOpen) return
            onRest()
            postDelayed(this, HOLD_POLL_MS)
        }
    }

    init {
        visibility = GONE
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
    }

    // ---- Opening, tracking, closing ----

    /** Opens `menu` around (x, y) (this view's coordinates). `fromLeft`: pulled from the left edge, opens right. */
    fun open(menu: List<Item>, x: Float, y: Float, sticky: Boolean, fromLeft: Boolean = false) {
        if (menu.isEmpty()) return
        root = menu
        parents.clear()
        this.sticky = sticky
        closing = false
        isClickable = sticky
        visibility = VISIBLE
        fingerX = x
        fingerY = y
        layout = RadialLayout(menu.size, radius, itemRadius, deadZone, opensRight = fromLeft)
        layout.place(x, y, width.toFloat(), height.toFloat(), dp(12f))
        show(menu)
        removeCallbacks(holdCheck)
        postDelayed(holdCheck, HOLD_POLL_MS)
    }

    /** The finger that opened the menu moved to (x, y). */
    fun drag(x: Float, y: Float) {
        fingerX = x
        fingerY = y
        setHighlight(layout.pick(x, y))
    }

    /** The finger that opened the menu lifted at (x, y): run what it points at, or close. */
    fun release(x: Float, y: Float) {
        if (sticky) return
        drag(x, y)
        val item = items.getOrNull(highlight)
        if (item != null && item.children.isEmpty()) choose(item) else close()
    }

    fun close() {
        if (visibility != VISIBLE || closing) return
        closing = true
        removeCallbacks(holdCheck)
        animate(progress, 0f, CLOSE_MS) {
            visibility = GONE
            closing = false
            isClickable = false
            highlight = -1
        }
    }

    private fun choose(item: Item) {
        performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
        close()
        onPick?.invoke(item)
    }

    /** Shows `level` (the root or an item's children) flying out from the center. */
    private fun show(level: List<Item>) {
        items = level
        layout = RadialLayout(level.size, radius, itemRadius, deadZone, layout.opensRight).also {
            it.place(layout.centerX, layout.centerY, width.toFloat(), height.toFloat(), dp(12f))
        }
        highlight = -1
        focus = 0f
        restSince = SystemClock.uptimeMillis()
        animate(0f, 1f, OPEN_MS, null)
    }

    /** Holding still: on an item with children opens them, in the center goes back a level. */
    private fun onRest() {
        if (SystemClock.uptimeMillis() - restSince < HOLD_MS || progress < 1f) return
        val item = items.getOrNull(highlight)
        if (item != null && item.children.isNotEmpty()) {
            enter(item)
        } else if (highlight < 0 && parents.isNotEmpty() && layout.pick(fingerX, fingerY) < 0 && nearCenter()) {
            back()
        }
    }

    private fun nearCenter(): Boolean =
        kotlin.math.hypot(fingerX - layout.centerX, fingerY - layout.centerY) < deadZone

    private fun enter(item: Item) {
        performHapticFeedback(HapticFeedbackConstants.CONTEXT_CLICK)
        parents.addLast(items)
        show(item.children)
    }

    private fun back() {
        performHapticFeedback(HapticFeedbackConstants.CONTEXT_CLICK)
        show(parents.removeLast())
    }

    private fun setHighlight(index: Int) {
        if (index == highlight) return
        highlight = index
        restSince = SystemClock.uptimeMillis()
        if (index >= 0) performHapticFeedback(HapticFeedbackConstants.CLOCK_TICK)
        focusAnimator?.cancel()
        focus = 0f
        focusAnimator = ValueAnimator.ofFloat(0f, 1f).apply {
            duration = FOCUS_MS
            interpolator = DecelerateInterpolator()
            addUpdateListener {
                focus = it.animatedValue as Float
                invalidate()
            }
            start()
        }
        invalidate()
    }

    private fun animate(from: Float, to: Float, ms: Long, end: (() -> Unit)?) {
        animator?.cancel()
        animator = ValueAnimator.ofFloat(from, to).apply {
            duration = ms
            addUpdateListener {
                progress = it.animatedValue as Float
                invalidate()
            }
            if (end != null) {
                addListener(object : android.animation.AnimatorListenerAdapter() {
                    override fun onAnimationEnd(animation: android.animation.Animator) = end()
                })
            }
            start()
        }
    }

    // ---- Sticky mode: taps ----

    @SuppressLint("ClickableViewAccessibility") // Taps pick drawn items; the actions also exist in the controls panel.
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (!sticky || !isOpen) return false
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_MOVE -> drag(event.x, event.y)
            MotionEvent.ACTION_UP -> {
                val item = items.getOrNull(layout.pick(event.x, event.y))
                when {
                    item == null && nearCenter() && parents.isNotEmpty() -> back()
                    item == null -> close()
                    item.children.isNotEmpty() -> enter(item)
                    else -> choose(item)
                }
            }
            MotionEvent.ACTION_CANCEL -> setHighlight(-1)
        }
        return true
    }

    // ---- Drawing ----

    override fun onDraw(canvas: Canvas) {
        if (visibility != VISIBLE || items.isEmpty()) return
        val p = progress.coerceIn(0f, 1f)
        val eased = 1f - (1f - p) * (1f - p) * (1f - p)
        scrimPaint.alpha = (SCRIM_ALPHA * p).toInt()
        canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), scrimPaint)

        val cx = layout.centerX
        val cy = layout.centerY
        // The track: a thick half ring under the items, swept open from the top.
        ringPaint.strokeWidth = itemRadius * 2f + dp(14f)
        ringPaint.alpha = (255 * min(1f, p * 1.6f)).toInt()
        arc.set(cx - radius, cy - radius, cx + radius, cy + radius)
        val sweep = 180f * eased * if (layout.opensRight) 1f else -1f
        canvas.drawArc(arc, -90f, sweep, false, ringPaint)
        ringEdgePaint.alpha = ringPaint.alpha / 2
        val outer = radius + ringPaint.strokeWidth / 2f
        arc.set(cx - outer, cy - outer, cx + outer, cy + outer)
        canvas.drawArc(arc, -90f, sweep, false, ringEdgePaint)

        for (i in items.indices) drawItem(canvas, i, p)
        items.getOrNull(highlight)?.let { drawLabel(canvas, highlight, it) }
    }

    private fun drawItem(canvas: Canvas, i: Int, p: Float) {
        val item = items[i]
        // Each item flies out a little after the one before it, overshooting slightly.
        val t = ((p - i * STAGGER) / (1f - STAGGER * (items.size - 1)).coerceAtLeast(0.3f)).coerceIn(0f, 1f)
        if (t <= 0f) return
        val pop = overshoot(t)
        val lit = i == highlight
        val r = radius * (0.55f + 0.45f * pop)
        val x = layout.itemX(i, r)
        val y = layout.itemY(i, r)
        val size = itemRadius * pop * (if (lit) 1f + 0.16f * focus else 1f)
        val alpha = (255 * t).toInt()

        itemPaint.color = when {
            lit -> accent
            item.active -> ACTIVE_FILL
            else -> ITEM_FILL
        }
        itemPaint.alpha = alpha
        canvas.drawCircle(x, y, size, itemPaint)
        itemEdgePaint.color = if (item.active && !lit) accent else ITEM_EDGE
        itemEdgePaint.alpha = alpha
        canvas.drawCircle(x, y, size, itemEdgePaint)

        val ink = if (lit) TEXT else if (item.active) accent else TEXT
        if (item.icon != 0) {
            val d = icons[item.icon] ?: ContextCompat.getDrawable(context, item.icon)?.mutate()?.also { icons[item.icon] = it } ?: return
            val half = (size * 0.44f).toInt()
            d.setTint(ink)
            d.alpha = alpha
            d.setBounds((x - half).toInt(), (y - half).toInt(), (x + half).toInt(), (y + half).toInt())
            d.draw(canvas)
        } else if (item.glyph != null) {
            glyphPaint.color = ink
            glyphPaint.alpha = alpha
            glyphPaint.textSize = dp(if (item.glyph.length > 3) 11f else 13f) * pop
            canvas.drawText(item.glyph, x, y - (glyphPaint.ascent() + glyphPaint.descent()) / 2f, glyphPaint)
        }
        // A small dot marks actions that open more.
        if (item.children.isNotEmpty()) {
            val dotR = r + size + dp(5f)
            itemPaint.color = if (lit) accent else TEXT_DIM
            itemPaint.alpha = alpha
            canvas.drawCircle(layout.itemX(i, dotR), layout.itemY(i, dotR), dp(2.2f), itemPaint)
        }
    }

    /** The highlighted action's name, just outside its circle, kept on screen. */
    private fun drawLabel(canvas: Canvas, i: Int, item: Item) {
        val a = (255 * focus).toInt()
        if (a <= 0) return
        val r = radius + itemRadius + dp(26f)
        val w = labelPaint.measureText(item.label) + dp(20f)
        val h = dp(28f)
        val x = layout.itemX(i, r).coerceIn(w / 2f + dp(8f), width - w / 2f - dp(8f))
        val y = layout.itemY(i, r).coerceIn(h / 2f + dp(8f), height - h / 2f - dp(8f))
        pill.set(x - w / 2f, y - h / 2f, x + w / 2f, y + h / 2f)
        labelBg.alpha = (LABEL_BG ushr 24) * a / 255
        canvas.drawRoundRect(pill, h / 2f, h / 2f, labelBg)
        labelPaint.alpha = a
        canvas.drawText(item.label, x, y - (labelPaint.ascent() + labelPaint.descent()) / 2f, labelPaint)
    }

    private fun overshoot(t: Float): Float {
        val s = 1.6f
        val u = t - 1f
        return u * u * ((s + 1f) * u + s) + 1f
    }

    private companion object {
        const val OPEN_MS = 260L
        const val CLOSE_MS = 140L
        const val FOCUS_MS = 120L
        const val HOLD_MS = 380L
        const val HOLD_POLL_MS = 60L
        const val STAGGER = 0.07f
        const val SCRIM_ALPHA = 90

        // A dark glass look over any remote screen, in both app themes.
        const val SCRIM = 0xFF000000.toInt()
        const val RING = 0xE61A1A1E.toInt()
        const val RING_EDGE = 0x33FFFFFF
        const val ITEM_FILL = 0xFF2A2A30.toInt()
        const val ACTIVE_FILL = 0xFF3A2025.toInt()
        const val ITEM_EDGE = 0x1FFFFFFF
        const val LABEL_BG = 0xF0141417.toInt()
        const val TEXT = 0xFFF4F4F6.toInt()
        const val TEXT_DIM = 0x99F4F4F6.toInt()
    }
}

/**
 * The grip at the screen's side edge that opens the [RadialMenuView]: hold it (or start sliding inward) and the menu
 * opens around the finger and follows it; a plain tap opens it sticky. The area is excluded from the system Back
 * gesture so holding there never leaves the session.
 */
class EdgeHandle(context: Context, private val menu: RadialMenuView, private val items: () -> List<RadialMenuView.Item>) :
    View(context) {

    /** Pulled from the left edge (the menu opens to the right). */
    var leftEdge = false

    private val density = resources.displayMetrics.density
    private val grip = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0xFFFFFFFF.toInt() }
    private val gripShadow = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0x66000000 }
    private val rect = RectF()
    private var downX = 0f
    private var downY = 0f
    private var opened = false
    private var closedOnDown = false
    private val location = IntArray(2)
    private val menuLocation = IntArray(2)
    private val exclusion = android.graphics.Rect()
    private val exclusionList = listOf(exclusion)

    private val openByHold = Runnable { openAt(downX, downY, sticky = false) }

    init {
        contentDescription = context.getString(R.string.radial_handle)
        alpha = IDLE_ALPHA
    }

    /** Briefly shows the grip at full strength, so a new session hints where the menu lives. */
    fun hint() {
        alpha = 1f
        animate().alpha(IDLE_ALPHA).setStartDelay(HINT_MS).setDuration(600L).start()
    }

    override fun onLayout(changed: Boolean, left: Int, top: Int, right: Int, bottom: Int) {
        super.onLayout(changed, left, top, right, bottom)
        if (changed && android.os.Build.VERSION.SDK_INT >= 29) {
            exclusion.set(0, 0, width, height)
            systemGestureExclusionRects = exclusionList
        }
    }

    override fun onDraw(canvas: Canvas) {
        val w = 4f * density
        val h = 56f * density
        val x = if (leftEdge) 6f * density else width - 6f * density - w
        val y = (height - h) / 2f
        rect.set(x - density, y - density, x + w + density, y + h + density)
        canvas.drawRoundRect(rect, w, w, gripShadow)
        rect.set(x, y, x + w, y + h)
        canvas.drawRoundRect(rect, w / 2f, w / 2f, grip)
    }

    @SuppressLint("ClickableViewAccessibility") // performClick opens the menu sticky, as a tap does.
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (closedOnDown && event.actionMasked != MotionEvent.ACTION_DOWN) return true
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                // A touch on the grip while the menu is open only closes it.
                closedOnDown = menu.isOpen
                if (closedOnDown) {
                    menu.close()
                    return true
                }
                downX = event.x
                downY = event.y
                opened = false
                animate().cancel()
                alpha = 1f
                postDelayed(openByHold, HOLD_MS)
            }
            MotionEvent.ACTION_MOVE -> {
                val inward = if (leftEdge) event.x - downX else downX - event.x
                if (!opened && inward > SLIDE_DP * density) {
                    removeCallbacks(openByHold)
                    openAt(event.x, event.y, sticky = false)
                }
                if (opened) {
                    val (x, y) = toMenu(event.x, event.y)
                    menu.drag(x, y)
                }
            }
            MotionEvent.ACTION_UP -> {
                removeCallbacks(openByHold)
                if (opened) {
                    val (x, y) = toMenu(event.x, event.y)
                    menu.release(x, y)
                } else {
                    performClick()
                }
                settle()
            }
            MotionEvent.ACTION_CANCEL -> {
                removeCallbacks(openByHold)
                if (opened) menu.close()
                settle()
            }
        }
        return true
    }

    override fun performClick(): Boolean {
        super.performClick()
        openAt(width / 2f, height / 2f, sticky = true)
        return true
    }

    private fun settle() {
        opened = false
        animate().alpha(IDLE_ALPHA).setStartDelay(0L).setDuration(200L).start()
    }

    private fun openAt(x: Float, y: Float, sticky: Boolean) {
        opened = !sticky
        performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
        val (mx, my) = toMenu(x, y)
        menu.open(items(), mx, my, sticky, fromLeft = leftEdge)
    }

    /** (x, y) in this view to the menu's coordinates. */
    private fun toMenu(x: Float, y: Float): Pair<Float, Float> {
        getLocationOnScreen(location)
        menu.getLocationOnScreen(menuLocation)
        return (x + location[0] - menuLocation[0]) to (y + location[1] - menuLocation[1])
    }

    private companion object {
        const val HOLD_MS = 220L
        const val SLIDE_DP = 16f
        const val IDLE_ALPHA = 0.35f
        const val HINT_MS = 1500L
    }
}
