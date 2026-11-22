package com.opendesk.app.ui.viewer.keyboard

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Rect
import android.graphics.RectF
import android.graphics.Typeface
import android.os.SystemClock
import android.util.AttributeSet
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.animation.AccelerateInterpolator
import android.view.animation.Interpolator
import androidx.core.content.ContextCompat
import androidx.core.content.res.ResourcesCompat
import com.opendesk.app.R
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Kind
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Mod
import com.opendesk.app.ui.viewer.keyboard.KeyInput.Output
import com.opendesk.app.ui.viewer.keyboard.KeyboardLayouts.Page
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.exp
import kotlin.math.roundToInt

/**
 * The mini glass keyboard: one view drawn on one canvas (no view per key) floating over the remote screen.
 * It spans its parent but takes only touches on its panel, so the remote screen around it stays usable.
 *
 * Look: the remote picture behind the panel, shrunk and blurred ([backdrop], at most 10 times a second) under a
 * dark tint, a 1 px highlight on the top edge, 22 dp corners; keys are faint white with white Inter text, Enter in
 * the OpenDesk accent. A strip on top holds the system-keyboard switch, paste (the phone's clipboard, shown when it
 * holds text), the split switch (landscape), a drag handle (moves the panel), the opacity switch, a resize grip (drag
 * it; two fingers on the strip pinch) and the hide button. Size, place, opacity and split are kept by the [Listener].
 *
 * Split (landscape): two panels at the bottom-left and bottom-right corners, the middle of the screen left free; the
 * left strip has the system keyboard, paste and split buttons, the right one opacity, grip and hide.
 */
class GlassKeyboardView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : View(context, attrs) {

    interface Listener {
        fun onText(text: String)
        fun onKey(key: Output.Key)
        /** Win on its own (the Start menu). */
        fun onWinKey()
        fun onSystemKeyboard()
        fun onHide()
        /** Paste the phone's clipboard text. */
        fun onPaste()
        /** The user switched the landscape keyboard between split and whole. */
        fun onSplitChanged(split: Boolean)
        /** The user moved, resized or dimmed the panel: keep it. */
        fun onPlacementChanged(placement: Placement)
    }

    /** Where the panel sits: `scale` of the default size, its center as a fraction of the width, and its bottom
     *  edge's distance from the view's bottom as a fraction of the height. */
    data class Placement(val scale: Float = 1f, val centerX: Float = 0.5f, val bottomGap: Float = -1f, val opacity: Float = 1f)

    var listener: Listener? = null

    /** Fills a bitmap with the remote picture of the whole view, scaled to the bitmap; false when there is none. */
    var backdrop: ((Bitmap) -> Boolean)? = null

    /** The focused host field hides what is typed: no key preview bubbles. */
    var password = false

    /** Landscape shows the split keyboard (portrait is always whole). */
    var split = false
        set(value) {
            if (field == value) return
            field = value
            relayout()
        }

    /** The phone's clipboard holds text: the paste button shows. */
    var pasteAvailable = false
        set(value) {
            if (field == value) return
            field = value
            relayout()
        }

    /**
     * Top edge of the keyboard (the higher panel) in view coordinates while it is up, or null while it is down or
     * going down. The remote screen keeps the focused field above it.
     */
    val keyboardTop: Float?
        get() = if (!isUp || panel.isEmpty) null else if (splitOn) minOf(panel.top, panelRight.top) else panel.top

    /** `(x, y)` (view coordinates) is on the keyboard while it is up. */
    fun onPanel(x: Float, y: Float): Boolean = isUp && (panel.contains(x, y) || panelRight.contains(x, y))

    var placement = Placement()
        set(value) {
            field = value.copy(scale = KeyboardLayouts.clampScale(value.scale))
            relayout()
        }

    val isUp: Boolean get() = visibility == VISIBLE && !hiding

    private val dp = resources.displayMetrics.density
    private val font: Typeface = ResourcesCompat.getFont(context, R.font.inter_medium) ?: Typeface.DEFAULT
    private val accent = ContextCompat.getColor(context, R.color.accent)

    private var page = Page.LETTERS
    private val shift = KeyInput.Shift()
    private val mods = KeyInput.Modifiers()
    private var keys: List<KeyboardLayouts.Placed> = emptyList()

    /** The panel and its strip, in view coordinates; split: the left half, the right one in [panelRight]. */
    private val panel = RectF()
    private val strip = RectF()
    private val panelRight = RectF()
    private val stripRight = RectF()
    private val btnSystem = RectF()
    private val btnPaste = RectF()
    private val btnSplit = RectF()
    private val btnOpacity = RectF()
    private val btnGrip = RectF()
    private val btnHide = RectF()
    private val panelPath = Path()
    private val panelRightPath = Path()
    /** Both panels (the area drawn). */
    private val bounds = RectF()
    private var keysLeft: List<KeyboardLayouts.Placed> = emptyList()
    private var keysRight: List<KeyboardLayouts.Placed> = emptyList()

    private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
    private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply { typeface = font; textAlign = Paint.Align.CENTER; color = Color.WHITE }
    private val bitmapPaint = Paint(Paint.FILTER_BITMAP_FLAG)
    private val scratch = RectF()

    // ---- Appearance animation ----
    private var appear = 0f
    private var hiding = false
    private var animator: ValueAnimator? = null

    // ---- Backdrop ----
    private var screenShot: Bitmap? = null
    /** Blurred backdrops of [panel] and [panelRight]. */
    private val glass = arrayOfNulls<Bitmap>(2)
    private var pixels = IntArray(0)
    private val refreshBackdrop = object : Runnable {
        override fun run() {
            if (visibility != VISIBLE) return
            updateBackdrop()
            postDelayed(this, BACKDROP_MS)
        }
    }

    // ---- Touches ----
    /** Keys held down, by pointer id. */
    private val pressed = HashMap<Int, KeyboardLayouts.Placed>()
    private var dragMode = Drag.NONE
    private var dragPointer = -1
    private var dragStartX = 0f
    private var dragStartY = 0f
    private var dragStartPlacement = Placement()
    private var pinchStartSpan = 0f
    /** Space swiped sideways: arrow keys instead of a space. */
    private var spaceStartX = 0f
    private var spaceSteps = 0
    private var spaceSwiping = false
    private var repeatCount = 0
    private val repeat = object : Runnable {
        override fun run() {
            if (pressed.values.none { it.key.kind == Kind.BACKSPACE }) return
            emit(KeyInput.output(pressed.values.first { it.key.kind == Kind.BACKSPACE }.key, false, mods))
            repeatCount++
            postDelayed(this, KeyInput.repeatDelayMs(repeatCount))
        }
    }

    private enum class Drag { NONE, MOVE, RESIZE, PINCH }

    init {
        visibility = GONE
        isClickable = false
    }

    // ---- Showing and hiding ----

    /** Slides the keyboard in (spring) or out. */
    fun setShown(shown: Boolean) {
        if (shown == isUp) return
        animator?.cancel()
        hiding = !shown
        if (shown) {
            visibility = VISIBLE
            relayout()
            removeCallbacks(refreshBackdrop)
            post(refreshBackdrop)
        } else {
            releaseAll()
        }
        animator = ValueAnimator.ofFloat(appear, if (shown) 1f else 0f).apply {
            duration = if (shown) SHOW_MS else HIDE_MS
            interpolator = if (shown) SPRING else AccelerateInterpolator()
            addUpdateListener {
                appear = it.animatedValue as Float
                invalidate()
            }
            if (!shown) {
                addListener(object : android.animation.AnimatorListenerAdapter() {
                    override fun onAnimationEnd(animation: android.animation.Animator) {
                        if (hiding) {
                            visibility = GONE
                            hiding = false
                            removeCallbacks(refreshBackdrop)
                        }
                    }
                })
            }
            start()
        }
    }

    override fun onDetachedFromWindow() {
        animator?.cancel()
        removeCallbacks(refreshBackdrop)
        removeCallbacks(repeat)
        super.onDetachedFromWindow()
    }

    // ---- Layout ----

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        relayout()
    }

    private val landscape: Boolean get() = width > height

    /** The split keyboard shows now. */
    private val splitOn: Boolean get() = split && landscape

    private fun relayout() {
        if (width == 0 || height == 0) return
        val scale = placement.scale
        val margin = if (landscape) 10 * dp else 0f
        val (fw, fh) = if (splitOn) KeyboardLayouts.splitSize() else KeyboardLayouts.defaultSize(landscape)
        val maxWidth = if (splitOn) (width - 3 * margin) / 2 else width.toFloat()
        val pw = (width * fw * scale).coerceAtMost(maxWidth)
        // Keys need some height whatever the scale; the strip is fixed.
        val ph = (height * fh * (0.5f + 0.5f * scale)).coerceAtLeast(150 * dp).coerceAtMost(height * 0.9f)
        val gap = if (placement.bottomGap >= 0f) placement.bottomGap * height else margin
        val bottom = (height - gap).coerceIn(ph, height.toFloat())
        val radius = CORNER_DP * dp
        val b = STRIP_DP * dp
        if (splitOn) {
            panel.set(margin, bottom - ph, margin + pw, bottom)
            panelRight.set(width - margin - pw, bottom - ph, width - margin, bottom)
        } else {
            val cx = (placement.centerX * width).coerceIn(pw / 2, width - pw / 2)
            panel.set(cx - pw / 2, bottom - ph, cx + pw / 2, bottom)
            panelRight.setEmpty()
        }
        panelPath.reset()
        panelPath.addRoundRect(panel, radius, radius, Path.Direction.CW)
        panelRightPath.reset()
        if (splitOn) panelRightPath.addRoundRect(panelRight, radius, radius, Path.Direction.CW)
        bounds.set(panel)
        if (splitOn) bounds.union(panelRight)
        strip.set(panel.left, panel.top, panel.right, panel.top + b)
        if (splitOn) stripRight.set(panelRight.left, panelRight.top, panelRight.right, panelRight.top + b) else stripRight.setEmpty()
        // Left end: system keyboard, paste (when there is text), split (landscape).
        var x = strip.left + 8 * dp
        btnSystem.set(x, strip.top, x + b * 1.3f, strip.bottom)
        x = btnSystem.right
        if (pasteAvailable) {
            btnPaste.set(x, strip.top, x + b * 1.2f, strip.bottom)
            x = btnPaste.right
        } else {
            btnPaste.setEmpty()
        }
        if (landscape) btnSplit.set(x, strip.top, x + b * 1.2f, strip.bottom) else btnSplit.setEmpty()
        // Right end (of the right half when split): opacity, grip, hide.
        val end = if (splitOn) stripRight else strip
        btnHide.set(end.right - 8 * dp - b * 1.2f, end.top, end.right - 8 * dp, end.bottom)
        btnGrip.set(btnHide.left - b * 1.2f, end.top, btnHide.left, end.bottom)
        btnOpacity.set(btnGrip.left - b * 1.2f, end.top, btnGrip.left, end.bottom)
        layoutKeys()
        invalidate()
    }

    private fun layoutKeys() {
        val pad = 6 * dp
        keys = if (splitOn) {
            KeyboardLayouts.placeSplit(
                KeyboardLayouts.splitRows(page),
                panel.left + pad,
                strip.bottom,
                panelRight.right - panel.left - 2 * pad,
                panel.bottom - strip.bottom - pad,
                panel.width() - 2 * pad,
                KEY_GAP_DP * dp,
            )
        } else {
            KeyboardLayouts.place(
                KeyboardLayouts.rows(page),
                panel.left + pad,
                strip.bottom,
                panel.width() - 2 * pad,
                panel.bottom - strip.bottom - pad,
                KEY_GAP_DP * dp,
            )
        }
        keysLeft = keys.filter { it.side == 0 }
        keysRight = keys.filter { it.side == 1 }
    }

    // ---- Drawing ----

    override fun onDraw(canvas: Canvas) {
        if (appear <= 0f || panel.isEmpty) return
        val slide = (1f - appear) * (panel.height() * 0.35f + 24 * dp)
        val alpha = (appear.coerceIn(0f, 1f) * placement.opacity * 255).roundToInt()
        canvas.save()
        canvas.translate(0f, slide)
        val layer = canvas.saveLayerAlpha(bounds.left, bounds.top - 80 * dp, bounds.right, bounds.bottom, alpha)
        drawPanel(canvas, panel, panelPath, glass[0])
        if (splitOn) drawPanel(canvas, panelRight, panelRightPath, glass[1])
        drawStrip(canvas)
        drawKeys(canvas)
        canvas.restoreToCount(layer)
        canvas.restore()
    }

    private fun drawPanel(canvas: Canvas, box: RectF, path: Path, backdrop: Bitmap?) {
        val radius = CORNER_DP * dp
        if (android.os.Build.VERSION.SDK_INT >= 28) {
            fill.setShadowLayer(18 * dp, 0f, 6 * dp, 0x40000000)
            fill.color = 0x01000000
            canvas.drawRoundRect(box, radius, radius, fill)
            fill.clearShadowLayer()
        }
        canvas.save()
        canvas.clipPath(path)
        backdrop?.let { canvas.drawBitmap(it, null, box, bitmapPaint) }
        fill.color = TINT
        canvas.drawRect(box, fill)
        canvas.restore()
        // 1 px highlight along the top edge, between the corners.
        fill.color = HIGHLIGHT
        canvas.drawRect(box.left + radius * 0.6f, box.top, box.right - radius * 0.6f, box.top + 1f, fill)
    }

    private fun drawStrip(canvas: Canvas) {
        // Handle pills (drag to move) in the free part of each strip.
        val leftEnd = maxOf(btnSystem.right, btnPaste.right, btnSplit.right)
        if (splitOn) {
            drawPill(canvas, (leftEnd + strip.right) / 2, strip)
            drawPill(canvas, (stripRight.left + btnOpacity.left) / 2, stripRight)
        } else {
            val free = btnOpacity.left - leftEnd
            drawPill(canvas, if (strip.centerX() - 24 * dp < leftEnd) leftEnd + free / 2 else strip.centerX(), strip)
        }
        val c = 0xCCFFFFFF.toInt()
        drawIcon(canvas, Icon.SYSTEM_KEYBOARD, btnSystem, c, 16 * dp)
        if (!btnPaste.isEmpty) drawIcon(canvas, Icon.PASTE, btnPaste, c, 15 * dp)
        if (!btnSplit.isEmpty) drawIcon(canvas, if (splitOn) Icon.MERGE else Icon.SPLIT, btnSplit, c, 16 * dp)
        drawIcon(canvas, Icon.OPACITY, btnOpacity, c, 14 * dp)
        drawIcon(canvas, Icon.GRIP, btnGrip, c, 14 * dp)
        drawIcon(canvas, Icon.HIDE, btnHide, c, 14 * dp)
    }

    private fun drawPill(canvas: Canvas, cx: Float, box: RectF) {
        fill.color = 0x66FFFFFF
        scratch.set(cx - 18 * dp, box.centerY() - 2.5f * dp, cx + 18 * dp, box.centerY() + 2.5f * dp)
        canvas.drawRoundRect(scratch, 3 * dp, 3 * dp, fill)
    }

    private enum class Icon { SYSTEM_KEYBOARD, PASTE, SPLIT, MERGE, OPACITY, GRIP, HIDE, BACKSPACE, ENTER, SHIFT, CAPS }

    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
    }
    private val iconPath = Path()

    /** Line icons drawn in a `size` square centered in `box` (no font has all of these glyphs). */
    private fun drawIcon(canvas: Canvas, icon: Icon, box: RectF, color: Int, size: Float) {
        val h = size / 2
        val cx = box.centerX()
        val cy = box.centerY()
        stroke.color = color
        stroke.strokeWidth = (size / 11).coerceAtLeast(1.4f * dp)
        fill.color = color
        val p = iconPath
        p.reset()
        when (icon) {
            Icon.SYSTEM_KEYBOARD -> {
                scratch.set(cx - h, cy - h * 0.62f, cx + h, cy + h * 0.62f)
                canvas.drawRoundRect(scratch, 2 * dp, 2 * dp, stroke)
                val dot = stroke.strokeWidth * 0.55f
                for (i in 0..3) {
                    val x = cx - h * 0.6f + i * h * 0.4f
                    canvas.drawCircle(x, cy - h * 0.2f, dot, fill)
                }
                canvas.drawLine(cx - h * 0.4f, cy + h * 0.25f, cx + h * 0.4f, cy + h * 0.25f, stroke)
            }
            Icon.PASTE -> {
                // A clipboard: board, clip on top, two text lines.
                scratch.set(cx - h * 0.72f, cy - h * 0.8f, cx + h * 0.72f, cy + h)
                canvas.drawRoundRect(scratch, 2 * dp, 2 * dp, stroke)
                scratch.set(cx - h * 0.32f, cy - h, cx + h * 0.32f, cy - h * 0.6f)
                canvas.drawRoundRect(scratch, 1.5f * dp, 1.5f * dp, fill)
                canvas.drawLine(cx - h * 0.35f, cy - h * 0.05f, cx + h * 0.35f, cy - h * 0.05f, stroke)
                canvas.drawLine(cx - h * 0.35f, cy + h * 0.4f, cx + h * 0.15f, cy + h * 0.4f, stroke)
            }
            Icon.SPLIT -> {
                // Two halves apart: split the keyboard.
                scratch.set(cx - h, cy - h * 0.55f, cx - h * 0.2f, cy + h * 0.55f)
                canvas.drawRoundRect(scratch, 1.5f * dp, 1.5f * dp, stroke)
                scratch.set(cx + h * 0.2f, cy - h * 0.55f, cx + h, cy + h * 0.55f)
                canvas.drawRoundRect(scratch, 1.5f * dp, 1.5f * dp, stroke)
            }
            Icon.MERGE -> {
                // One whole keyboard: join the halves.
                scratch.set(cx - h, cy - h * 0.55f, cx + h, cy + h * 0.55f)
                canvas.drawRoundRect(scratch, 1.5f * dp, 1.5f * dp, stroke)
                canvas.drawLine(cx - h * 0.45f, cy + h * 0.15f, cx + h * 0.45f, cy + h * 0.15f, stroke)
            }
            Icon.OPACITY -> {
                canvas.drawCircle(cx, cy, h * 0.85f, stroke)
                scratch.set(cx - h * 0.85f, cy - h * 0.85f, cx + h * 0.85f, cy + h * 0.85f)
                canvas.drawArc(scratch, 90f, 180f, true, fill)
            }
            Icon.GRIP -> {
                // Two arrow heads on a diagonal: drag to resize.
                canvas.drawLine(cx - h * 0.7f, cy + h * 0.7f, cx + h * 0.7f, cy - h * 0.7f, stroke)
                p.moveTo(cx + h * 0.1f, cy - h * 0.7f); p.lineTo(cx + h * 0.7f, cy - h * 0.7f); p.lineTo(cx + h * 0.7f, cy - h * 0.1f)
                p.moveTo(cx - h * 0.1f, cy + h * 0.7f); p.lineTo(cx - h * 0.7f, cy + h * 0.7f); p.lineTo(cx - h * 0.7f, cy + h * 0.1f)
                canvas.drawPath(p, stroke)
            }
            Icon.HIDE -> {
                p.moveTo(cx - h * 0.7f, cy - h * 0.3f); p.lineTo(cx, cy + h * 0.35f); p.lineTo(cx + h * 0.7f, cy - h * 0.3f)
                canvas.drawPath(p, stroke)
            }
            Icon.BACKSPACE -> {
                p.moveTo(cx - h, cy); p.lineTo(cx - h * 0.45f, cy - h * 0.6f); p.lineTo(cx + h, cy - h * 0.6f)
                p.lineTo(cx + h, cy + h * 0.6f); p.lineTo(cx - h * 0.45f, cy + h * 0.6f); p.close()
                canvas.drawPath(p, stroke)
                val x = cx + h * 0.22f
                canvas.drawLine(x - h * 0.25f, cy - h * 0.25f, x + h * 0.25f, cy + h * 0.25f, stroke)
                canvas.drawLine(x - h * 0.25f, cy + h * 0.25f, x + h * 0.25f, cy - h * 0.25f, stroke)
            }
            Icon.ENTER -> {
                p.moveTo(cx + h * 0.8f, cy - h * 0.7f); p.lineTo(cx + h * 0.8f, cy + h * 0.2f); p.lineTo(cx - h * 0.8f, cy + h * 0.2f)
                p.moveTo(cx - h * 0.4f, cy - h * 0.2f); p.lineTo(cx - h * 0.8f, cy + h * 0.2f); p.lineTo(cx - h * 0.4f, cy + h * 0.6f)
                canvas.drawPath(p, stroke)
            }
            Icon.SHIFT, Icon.CAPS -> {
                p.moveTo(cx, cy - h * 0.85f); p.lineTo(cx + h * 0.8f, cy); p.lineTo(cx + h * 0.35f, cy)
                p.lineTo(cx + h * 0.35f, cy + h * 0.5f); p.lineTo(cx - h * 0.35f, cy + h * 0.5f); p.lineTo(cx - h * 0.35f, cy)
                p.lineTo(cx - h * 0.8f, cy); p.close()
                canvas.drawPath(p, if (shift.isOn) fill else stroke)
                if (icon == Icon.CAPS) canvas.drawLine(cx - h * 0.35f, cy + h * 0.85f, cx + h * 0.35f, cy + h * 0.85f, stroke)
            }
        }
    }

    private fun drawLabel(canvas: Canvas, label: String, box: RectF, size: Float = text.textSize) {
        text.textSize = size
        canvas.drawText(label, box.centerX(), box.centerY() - (text.descent() + text.ascent()) / 2, text)
    }

    private fun drawKeys(canvas: Canvas) {
        val r = KEY_RADIUS_DP * dp
        val held = pressed.values.toSet()
        var bubble: KeyboardLayouts.Placed? = null
        for (p in keys) {
            val isHeld = p in held
            val key = p.key
            canvas.save()
            if (isHeld) canvas.scale(PRESS_SCALE, PRESS_SCALE, p.centerX, (p.top + p.bottom) / 2)
            val active = when (key.kind) {
                Kind.SHIFT -> shift.isOn
                Kind.MOD -> mods.isOn(key.mod!!)
                else -> false
            }
            fill.color = when {
                key.kind == Kind.ENTER -> if (isHeld) blend(accent, Color.WHITE, 0.25f) else accent
                active -> 0xE6FFFFFF.toInt()
                isHeld -> 0x55FFFFFF
                key.isFunction -> 0x14FFFFFF
                else -> 0x22FFFFFF
            }
            scratch.set(p.left, p.top, p.right, p.bottom)
            canvas.drawRoundRect(scratch, r, r, fill)
            text.color = if (active) 0xFF1C1C1E.toInt() else Color.WHITE
            val label = when {
                key.kind == Kind.CHAR && shift.isOn -> key.label.uppercase()
                key.kind == Kind.SPACE -> "space"
                else -> key.label
            }
            val icon = when (key.kind) {
                Kind.BACKSPACE -> Icon.BACKSPACE
                Kind.ENTER -> Icon.ENTER
                Kind.SHIFT -> if (shift.state == KeyInput.Shift.State.LOCKED) Icon.CAPS else Icon.SHIFT
                else -> null
            }
            val size = when (key.kind) {
                Kind.CHAR -> (p.height * 0.46f).coerceAtMost(22 * dp)
                Kind.SPACE -> (p.height * 0.3f).coerceAtMost(13 * dp)
                else -> (p.height * 0.36f).coerceAtMost(15 * dp)
            }
            if (key.kind == Kind.SPACE) text.color = 0x88FFFFFF.toInt()
            if (icon != null) {
                drawIcon(canvas, icon, scratch, text.color, (p.height * 0.42f).coerceAtMost(20 * dp))
            } else {
                drawLabel(canvas, label, scratch, size)
            }
            // Locked modifier: a dot under the label.
            if ((key.kind == Kind.MOD && mods.isLocked(key.mod!!))) {
                fill.color = 0xFF1C1C1E.toInt()
                canvas.drawCircle(p.centerX, p.bottom - 4 * dp, 1.8f * dp, fill)
            }
            canvas.restore()
            if (isHeld && key.kind == Kind.CHAR && !password) bubble = p
        }
        text.color = Color.WHITE
        bubble?.let { drawBubble(canvas, it) }
    }

    /** The preview above a pressed letter. */
    private fun drawBubble(canvas: Canvas, p: KeyboardLayouts.Placed) {
        val w = p.width * 1.5f
        val h = p.height * 1.25f
        scratch.set(p.centerX - w / 2, p.top - h - 4 * dp, p.centerX + w / 2, p.top - 4 * dp)
        val box = if (p.side == 1) panelRight else panel
        scratch.offset((box.left - scratch.left).coerceAtLeast(0f) + (box.right - scratch.right).coerceAtMost(0f), 0f)
        fill.color = 0xF2F5F5F7.toInt()
        canvas.drawRoundRect(scratch, 10 * dp, 10 * dp, fill)
        text.color = 0xFF1C1C1E.toInt()
        drawLabel(canvas, if (shift.isOn) p.key.label.uppercase() else p.key.label, scratch, (h * 0.5f).coerceAtMost(30 * dp))
        text.color = Color.WHITE
    }

    // ---- Backdrop ----

    private fun updateBackdrop() {
        val source = backdrop ?: return
        if (width == 0 || height == 0 || panel.isEmpty) return
        val sw = (width / BACKDROP_SCALE).coerceAtLeast(1)
        val sh = (height / BACKDROP_SCALE).coerceAtLeast(1)
        val shot = screenShot?.takeIf { it.width == sw && it.height == sh }
            ?: Bitmap.createBitmap(sw, sh, Bitmap.Config.ARGB_8888).also { screenShot = it }
        if (!source(shot)) return
        blurInto(0, shot, panel)
        if (splitOn) blurInto(1, shot, panelRight) else glass[1] = null
        invalidate()
    }

    /** Crops `box` out of the small screenshot and blurs it into backdrop `slot`. */
    private fun blurInto(slot: Int, shot: Bitmap, box: RectF) {
        val sw = shot.width
        val sh = shot.height
        val crop = Rect(
            (box.left / BACKDROP_SCALE).toInt().coerceIn(0, sw - 1),
            (box.top / BACKDROP_SCALE).toInt().coerceIn(0, sh - 1),
            (box.right / BACKDROP_SCALE).toInt().coerceIn(1, sw),
            (box.bottom / BACKDROP_SCALE).toInt().coerceIn(1, sh),
        )
        val gw = crop.width().coerceIn(1, sw - crop.left)
        val gh = crop.height().coerceIn(1, sh - crop.top)
        val out = glass[slot]?.takeIf { it.width == gw && it.height == gh }
            ?: Bitmap.createBitmap(gw, gh, Bitmap.Config.ARGB_8888).also { glass[slot] = it }
        if (pixels.size != gw * gh) pixels = IntArray(gw * gh)
        shot.getPixels(pixels, 0, gw, crop.left, crop.top, gw, gh)
        GlassBlur.blur(pixels, gw, gh, BLUR_RADIUS)
        out.setPixels(pixels, 0, gw, 0, 0, gw, gh)
    }

    // ---- Touches ----

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (!isUp) return false
        val i = event.actionIndex
        val id = event.getPointerId(i)
        val x = event.getX(i)
        val y = event.getY(i)
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                if (!panel.contains(x, y) && !panelRight.contains(x, y)) return event.actionMasked != MotionEvent.ACTION_DOWN
                if (strip.contains(x, y) || stripRight.contains(x, y)) {
                    stripDown(event, id, x, y)
                } else {
                    keyDown(id, x, y)
                }
                return true
            }
            MotionEvent.ACTION_MOVE -> {
                onMove(event)
                return true
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                if (id == dragPointer || dragMode == Drag.PINCH) endDrag(x, y, id) else keyUp(id)
                return true
            }
            MotionEvent.ACTION_CANCEL -> {
                releaseAll()
                return true
            }
        }
        return true
    }

    private fun stripDown(event: MotionEvent, id: Int, x: Float, y: Float) {
        if (dragMode == Drag.MOVE && event.pointerCount == 2) {
            // A second finger on the strip: pinch to resize.
            dragMode = Drag.PINCH
            pinchStartSpan = span(event)
            dragStartPlacement = placement
            return
        }
        if (dragMode != Drag.NONE) return
        dragPointer = id
        dragStartX = x
        dragStartY = y
        dragStartPlacement = placement
        dragMode = when {
            btnGrip.contains(x, y) -> Drag.RESIZE
            isButton(x, y) -> Drag.NONE
            else -> Drag.MOVE
        }
        if (dragMode == Drag.NONE) dragPointer = id // a button: acts on release
    }

    private fun isButton(x: Float, y: Float): Boolean =
        btnSystem.contains(x, y) || btnPaste.contains(x, y) || btnSplit.contains(x, y) ||
            btnHide.contains(x, y) || btnOpacity.contains(x, y)

    private fun span(event: MotionEvent): Float =
        if (event.pointerCount < 2) 0f else abs(event.getX(0) - event.getX(1)).coerceAtLeast(1f)

    private fun onMove(event: MotionEvent) {
        when (dragMode) {
            Drag.PINCH -> if (event.pointerCount >= 2 && pinchStartSpan > 0f) {
                placement = dragStartPlacement.copy(scale = dragStartPlacement.scale * span(event) / pinchStartSpan)
            }
            Drag.MOVE, Drag.RESIZE -> {
                val index = event.findPointerIndex(dragPointer)
                if (index < 0) return
                val dx = event.getX(index) - dragStartX
                val dy = event.getY(index) - dragStartY
                placement = if (dragMode == Drag.MOVE) {
                    val start = dragStartPlacement
                    val startGap = if (start.bottomGap >= 0f) start.bottomGap else (height - panel.bottom) / height
                    start.copy(
                        // Split halves stay in their corners: they move up and down only.
                        centerX = if (splitOn) start.centerX else ((start.centerX * width + dx) / width).coerceIn(0f, 1f),
                        bottomGap = ((startGap * height - dy) / height).coerceIn(0f, 0.9f),
                    )
                } else {
                    // Right and up grow the panel (it stays centered on the same point); split: towards the middle
                    // (left, the grip is on the right half) and up grow both halves.
                    val (fw, _) = if (splitOn) KeyboardLayouts.splitSize() else KeyboardLayouts.defaultSize(landscape)
                    val base = width * fw
                    val grow = if (splitOn) -dx else 2 * dx
                    dragStartPlacement.copy(scale = dragStartPlacement.scale + (grow - dy) / base)
                }
            }
            Drag.NONE -> moveKeys(event)
        }
    }

    /** A finger on Space slid sideways: one arrow key per step. */
    private fun moveKeys(event: MotionEvent) {
        for (index in 0 until event.pointerCount) {
            val p = pressed[event.getPointerId(index)] ?: continue
            if (p.key.kind != Kind.SPACE) continue
            val steps = ((event.getX(index) - spaceStartX) / (SPACE_STEP_DP * dp)).toInt()
            if (steps != 0 && !spaceSwiping) spaceSwiping = true
            while (spaceSwiping && spaceSteps != steps) {
                val right = steps > spaceSteps
                spaceSteps += if (right) 1 else -1
                listener?.onKey(Output.Key(if (right) com.opendesk.app.domain.WindowsKeys.VK_RIGHT else com.opendesk.app.domain.WindowsKeys.VK_LEFT, shift = shift.isOn))
            }
        }
    }

    private fun endDrag(x: Float, y: Float, id: Int) {
        val mode = dragMode
        if (mode == Drag.PINCH && id != dragPointer) {
            // One finger of the pinch lifted: keep moving with the other.
            dragMode = Drag.NONE
            dragPointer = -1
            listener?.onPlacementChanged(placement)
            return
        }
        dragMode = Drag.NONE
        dragPointer = -1
        when {
            mode == Drag.MOVE || mode == Drag.RESIZE || mode == Drag.PINCH -> listener?.onPlacementChanged(placement)
            btnSystem.contains(x, y) -> listener?.onSystemKeyboard()
            btnHide.contains(x, y) -> listener?.onHide()
            btnPaste.contains(x, y) -> listener?.onPaste()
            btnSplit.contains(x, y) -> {
                split = !split
                listener?.onSplitChanged(split)
            }
            btnOpacity.contains(x, y) -> {
                val next = OPACITIES[(OPACITIES.indexOfFirst { abs(it - placement.opacity) < 0.01f } + 1) % OPACITIES.size]
                placement = placement.copy(opacity = next)
                listener?.onPlacementChanged(placement)
            }
        }
        if (mode != Drag.NONE || isButton(x, y)) tick()
    }

    private fun keyDown(id: Int, x: Float, y: Float) {
        // Split: only the touched half's keys (a touch near the middle must not reach across).
        val candidates = if (splitOn) (if (panelRight.contains(x, y)) keysRight else keysLeft) else keys
        val p = KeyboardLayouts.hit(candidates, x, y, KEY_GAP_DP * dp) ?: return
        // Rolling over to the next key: the one still held is typed now.
        pressed.keys.toList().forEach { other -> if (pressed[other]?.key?.kind == Kind.CHAR) keyUp(other) }
        pressed[id] = p
        tick()
        when (p.key.kind) {
            Kind.BACKSPACE -> {
                emit(KeyInput.output(p.key, false, mods))
                repeatCount = 0
                removeCallbacks(repeat)
                postDelayed(repeat, KeyInput.repeatDelayMs(0))
            }
            Kind.SPACE -> {
                spaceStartX = x
                spaceSteps = 0
                spaceSwiping = false
            }
            else -> Unit
        }
        invalidate()
    }

    private fun keyUp(id: Int) {
        val p = pressed.remove(id) ?: return
        val key = p.key
        val now = SystemClock.uptimeMillis()
        when (key.kind) {
            Kind.SHIFT -> shift.tap(now)
            Kind.MOD -> if (mods.tap(key.mod!!, now)) listener?.onWinKey()
            Kind.PAGE -> {
                page = key.page!!
                shift.reset()
                layoutKeys()
            }
            Kind.BACKSPACE -> removeCallbacks(repeat)
            Kind.SPACE -> if (!spaceSwiping) emit(KeyInput.output(key, shift.isOn, mods)) else mods.consume()
            else -> emit(KeyInput.output(key, shift.isOn, mods))
        }
        invalidate()
    }

    /** Sends one key's output and uses up one-shot Shift and modifiers. */
    private fun emit(out: Output?) {
        when (out) {
            is Output.Text -> listener?.onText(out.text)
            is Output.Key -> listener?.onKey(out)
            null -> return
        }
        shift.consume()
        mods.consume()
        invalidate()
    }

    private fun releaseAll() {
        pressed.clear()
        removeCallbacks(repeat)
        dragMode = Drag.NONE
        dragPointer = -1
        invalidate()
    }

    private fun tick() {
        performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
    }

    /** Mods and Shift back to off (the keyboard was closed, or a new session). */
    fun resetModifiers() {
        shift.reset()
        mods.reset()
        page = Page.LETTERS
        layoutKeys()
        invalidate()
    }

    private companion object {
        const val CORNER_DP = 22f
        const val KEY_RADIUS_DP = 8f
        const val KEY_GAP_DP = 5f
        const val STRIP_DP = 30f
        const val SPACE_STEP_DP = 14f
        const val PRESS_SCALE = 0.94f
        const val SHOW_MS = 300L
        const val HIDE_MS = 160L
        const val BACKDROP_MS = 100L
        const val BACKDROP_SCALE = 8
        const val BLUR_RADIUS = 3

        /** Dark tint, about half opaque. */
        const val TINT = 0x8C121216.toInt()
        /** White 14 %. */
        const val HIGHLIGHT = 0x24FFFFFF
        val OPACITIES = floatArrayOf(1f, 0.8f, 0.6f)

        /** Damped spring (damping ratio ~0.8): a slight overshoot that settles. */
        val SPRING = Interpolator { t -> 1f - (exp(-5.5f * t) * cos(7.5f * t)) * (1f - t) }

        fun blend(a: Int, b: Int, f: Float): Int {
            val inv = 1f - f
            return Color.argb(
                (Color.alpha(a) * inv + Color.alpha(b) * f).roundToInt(),
                (Color.red(a) * inv + Color.red(b) * f).roundToInt(),
                (Color.green(a) * inv + Color.green(b) * f).roundToInt(),
                (Color.blue(a) * inv + Color.blue(b) * f).roundToInt(),
            )
        }
    }
}
