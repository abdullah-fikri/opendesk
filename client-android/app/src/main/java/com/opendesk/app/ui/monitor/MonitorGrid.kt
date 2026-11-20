package com.opendesk.app.ui.monitor

import android.content.Context
import android.graphics.Bitmap
import android.graphics.drawable.GradientDrawable
import android.text.TextUtils
import android.view.GestureDetector
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.content.res.ResourcesCompat
import com.opendesk.app.R
import com.opendesk.app.domain.MonitorWall
import com.opendesk.app.ui.common.Ui
import kotlin.math.abs
import kotlin.math.ceil

/**
 * The wall's tiles: [slots] tiles in [columns] columns (the user's grid setting), as large as the space allows at
 * 16:10. Handles the gestures itself: a horizontal swipe turns the page, a double tap on a tile opens that computer.
 */
class MonitorGrid(context: Context) : ViewGroup(context) {
    /** Swipe: -1 (previous page) or +1 (next page). */
    var onSwipe: ((Int) -> Unit)? = null

    /** Double tap on the tile at this index. */
    var onDoubleTap: ((Int) -> Unit)? = null

    /** Single tap anywhere. */
    var onTap: (() -> Unit)? = null

    private val gap = Ui.dp(context, 6f)

    /** Tiles per row (the wall's grid setting). */
    var columns: Int = 1
        set(value) {
            if (field == value) return
            field = value
            requestLayout()
        }
    private var tileW = 0
    private var tileH = 0

    val tiles: List<MonitorTileView> get() = (0 until childCount).map { getChildAt(it) as MonitorTileView }

    /** Number of tile places on a page (1, 4, 9 or 16); tiles past the page's hosts are hidden. */
    var slots: Int = 0
        set(value) {
            if (field == value) return
            field = value
            removeAllViews()
            repeat(value) { addView(MonitorTileView(context)) }
            requestLayout()
        }

    private val gestures = GestureDetector(context, object : GestureDetector.SimpleOnGestureListener() {
        override fun onDown(e: MotionEvent): Boolean = true

        override fun onSingleTapConfirmed(e: MotionEvent): Boolean {
            onTap?.invoke()
            return true
        }

        override fun onDoubleTap(e: MotionEvent): Boolean {
            val index = tileAt(e.x, e.y)
            if (index >= 0) onDoubleTap?.invoke(index)
            return true
        }

        override fun onFling(e1: MotionEvent?, e2: MotionEvent, velocityX: Float, velocityY: Float): Boolean {
            val start = e1 ?: return false
            val dx = e2.x - start.x
            if (abs(dx) < Ui.dp(context, 48f) || abs(dx) < abs(e2.y - start.y)) return false
            onSwipe?.invoke(if (dx < 0) 1 else -1)
            return true
        }
    })

    @android.annotation.SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(event: MotionEvent): Boolean = gestures.onTouchEvent(event)

    private fun tileAt(x: Float, y: Float): Int {
        for (i in 0 until childCount) {
            val c = getChildAt(i)
            if (c.visibility == View.VISIBLE && x >= c.left && x < c.right && y >= c.top && y < c.bottom) return i
        }
        return -1
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val w = MeasureSpec.getSize(widthMeasureSpec)
        val h = MeasureSpec.getSize(heightMeasureSpec)
        val n = slots.coerceAtLeast(1)
        val best = MonitorWall.tileWidth(w, h, columns, n, gap)
        tileW = best.toInt().coerceAtLeast(0)
        tileH = (best / MonitorWall.TILE_ASPECT).toInt().coerceAtLeast(0)
        val spec = { size: Int -> MeasureSpec.makeMeasureSpec(size, MeasureSpec.EXACTLY) }
        for (i in 0 until childCount) getChildAt(i).measure(spec(tileW), spec(tileH))
        setMeasuredDimension(w, h)
    }

    override fun onLayout(changed: Boolean, l: Int, t: Int, r: Int, b: Int) {
        val n = slots.coerceAtLeast(1)
        val cols = columns.coerceIn(1, n)
        val rows = ceil(n / cols.toFloat()).toInt()
        val blockW = cols * tileW + (cols - 1) * gap
        val blockH = rows * tileH + (rows - 1) * gap
        val x0 = ((r - l) - blockW) / 2
        val y0 = ((b - t) - blockH) / 2
        for (i in 0 until childCount) {
            val x = x0 + (i % cols) * (tileW + gap)
            val y = y0 + (i / cols) * (tileH + gap)
            getChildAt(i).layout(x, y, x + tileW, y + tileH)
        }
    }
}

/** One tile: the thumbnail (or a status line while there is none) and the computer's name with a status dot. */
class MonitorTileView(context: Context) : FrameLayout(context) {
    private val image = ImageView(context).apply {
        scaleType = ImageView.ScaleType.FIT_CENTER
        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
    }
    private val status = TextView(context).apply {
        gravity = Gravity.CENTER
        setTextColor(Ui.color(context, R.color.wall_text2))
        textSize = 13f
        setPadding(Ui.dp(context, 8f), 0, Ui.dp(context, 8f), 0)
    }
    private val dot = View(context)
    private val name = TextView(context).apply {
        setTextColor(Ui.color(context, R.color.wall_text))
        textSize = 12f
        maxLines = 1
        ellipsize = TextUtils.TruncateAt.END
        typeface = ResourcesCompat.getFont(context, R.font.inter_semibold)
    }

    init {
        background = GradientDrawable().apply {
            setColor(Ui.color(context, R.color.wall_tile))
            cornerRadius = Ui.dp(context, 8f).toFloat()
        }
        clipToOutline = true
        addView(image, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(status, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        val label = LinearLayout(context).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(Ui.dp(context, 8f), Ui.dp(context, 4f), Ui.dp(context, 8f), Ui.dp(context, 4f))
            setBackgroundColor(Ui.color(context, R.color.wall_label))
            val size = Ui.dp(context, 7f)
            addView(dot, LinearLayout.LayoutParams(size, size).apply { marginEnd = Ui.dp(context, 6f) })
            addView(name, LinearLayout.LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f))
        }
        addView(label, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT, Gravity.BOTTOM))
    }

    /** Fills the tile: `picture` when there is one, else `statusText`; `live` colours the dot. */
    fun bind(title: String, live: Boolean, picture: Bitmap?, statusText: String) {
        name.text = title
        contentDescription = "$title, $statusText"
        dot.background = GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(if (live) Ui.color(context, R.color.green) else Ui.color(context, R.color.offline))
        }
        image.setImageBitmap(picture)
        image.visibility = if (picture != null) View.VISIBLE else View.INVISIBLE
        status.text = statusText
        status.visibility = if (picture == null) View.VISIBLE else View.GONE
    }
}
