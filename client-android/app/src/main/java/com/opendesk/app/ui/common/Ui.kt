package com.opendesk.app.ui.common

import android.content.Context
import android.graphics.Typeface
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.RelativeSizeSpan
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import androidx.annotation.ColorRes
import androidx.annotation.DrawableRes
import androidx.annotation.LayoutRes
import androidx.appcompat.widget.SwitchCompat
import androidx.core.content.ContextCompat
import androidx.core.content.res.ResourcesCompat
import com.opendesk.app.R

/** Small view helpers shared by the main screen's pages. */
internal object Ui {

    fun dp(context: Context, value: Float): Int = (value * context.resources.displayMetrics.density + 0.5f).toInt()

    fun color(context: Context, @ColorRes id: Int): Int = ContextCompat.getColor(context, id)

    fun mono(context: Context): Typeface? = ResourcesCompat.getFont(context, R.font.jetbrains_mono)

    fun inflate(parent: ViewGroup, @LayoutRes layout: Int): View =
        LayoutInflater.from(parent.context).inflate(layout, parent, false)

    /** Section caption above a card ("Online · 10"). */
    fun section(parent: ViewGroup, text: String): TextView {
        val view = TextView(parent.context, null, 0, R.style.Od_Section)
        view.text = text
        view.layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
            topMargin = dp(parent.context, 20f)
            bottomMargin = dp(parent.context, 8f)
        }
        return view
    }

    /** A rounded card holding rows, separated by inset hairlines. */
    fun card(context: Context): LinearLayout = LinearLayout(context).apply {
        orientation = LinearLayout.VERTICAL
        setBackgroundResource(R.drawable.card)
        elevation = dp(context, 1f).toFloat()
        clipToOutline = true
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
    }

    /** Adds `row` to `card`, with a hairline before it when it is not the first row. */
    fun addRow(card: LinearLayout, row: View, insetDp: Float = 62f) {
        if (card.childCount > 0) {
            card.addView(
                View(card.context).apply { setBackgroundColor(color(context, R.color.line)) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(card.context, 1f)).apply {
                    marginStart = dp(card.context, insetDp)
                },
            )
        }
        card.addView(row)
    }

    /** "● 10 online   ● 3 offline": small coloured dots before each count. */
    fun dotted(context: Context, vararg parts: Pair<String, Int>): CharSequence {
        val out = SpannableStringBuilder()
        parts.forEachIndexed { i, (text, dotColor) ->
            if (i > 0) out.append("   ")
            val start = out.length
            out.append("● ")
            out.setSpan(ForegroundColorSpan(color(context, dotColor)), start, start + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            out.setSpan(RelativeSizeSpan(0.7f), start, start + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            out.append(text)
        }
        return out
    }

    /** Fills a `row_setting` include. */
    fun settingRow(
        row: View,
        @DrawableRes tile: Int,
        @DrawableRes icon: Int,
        title: CharSequence,
        sub: CharSequence?,
        chevron: Boolean = false,
        onClick: (() -> Unit)? = null,
    ) {
        row.findViewById<View>(R.id.set_tile).setBackgroundResource(tile)
        row.findViewById<ImageView>(R.id.set_icon).setImageResource(icon)
        row.findViewById<TextView>(R.id.set_title).text = title
        row.findViewById<TextView>(R.id.set_sub).apply {
            text = sub
            visibility = if (sub.isNullOrEmpty()) View.GONE else View.VISIBLE
        }
        row.findViewById<View>(R.id.set_chev).visibility = if (chevron) View.VISIBLE else View.GONE
        if (onClick != null) {
            row.setOnClickListener { onClick() }
        } else {
            row.background = null
            row.isClickable = false
        }
    }

    /** One option of a segmented control: raised and bold when chosen. */
    fun segment(view: TextView, on: Boolean) {
        val context = view.context
        view.setBackgroundResource(if (on) R.drawable.seg_on else 0)
        val tone = color(context, if (on) R.color.text else R.color.text3)
        view.setTextColor(tone)
        androidx.core.widget.TextViewCompat.setCompoundDrawableTintList(view, android.content.res.ColorStateList.valueOf(tone))
        view.typeface = ResourcesCompat.getFont(context, if (on) R.font.inter_semibold else R.font.inter_regular)
        val size = dp(context, 16f)
        view.compoundDrawablesRelative[0]?.let { icon ->
            icon.setBounds(0, 0, size, size)
            view.setCompoundDrawablesRelative(icon, null, null, null)
        }
    }

    fun settingSwitch(row: View): SwitchCompat = row.findViewById<SwitchCompat>(R.id.set_switch).also { it.visibility = View.VISIBLE }

    /** "Windows" / "Linux" / "Android" from a relay `platform` ("windows-pc", "linux-admin", ...). */
    fun platformLabel(platform: String?): String {
        val p = platform.orEmpty()
        return when {
            p.startsWith("windows") -> "Windows"
            p.startsWith("linux") -> "Linux"
            p.startsWith("mac") -> "macOS"
            p == "android" -> "Android"
            else -> ""
        }
    }

    /** "24:18" (minutes:seconds) or "1:02:03". */
    fun duration(seconds: Long): String {
        val h = seconds / 3600
        val m = (seconds % 3600) / 60
        val s = seconds % 60
        return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%02d:%02d".format(m, s)
    }
}
