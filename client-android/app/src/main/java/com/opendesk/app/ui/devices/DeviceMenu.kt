package com.opendesk.app.ui.devices

import android.app.Activity
import android.app.Dialog
import android.graphics.RenderEffect
import android.graphics.Shader
import android.os.Build
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.view.animation.OvershootInterpolator
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import androidx.annotation.DrawableRes
import androidx.core.content.res.ResourcesCompat
import androidx.core.view.WindowCompat
import com.opendesk.app.R
import com.opendesk.app.ui.common.Ui

/**
 * Long-press menu for a computer, in the manner of an iOS context menu: the screen behind blurs and dims, the row
 * lifts where it was, and a rounded menu (Connect, Rename, Delete in red) opens under it, or above it near the
 * bottom of the screen. A tap outside closes it.
 */
object DeviceMenu {

    data class Action(val label: String, @DrawableRes val icon: Int, val destructive: Boolean = false, val run: () -> Unit)

    fun show(activity: Activity, anchor: View, preview: View, actions: List<Action>) {
        if (activity.isFinishing || activity.isDestroyed) return
        val dialog = Dialog(activity, R.style.Theme_OpenDesk_Menu)
        val window = dialog.window ?: return
        WindowCompat.setDecorFitsSystemWindows(window, false)

        val root = FrameLayout(activity).apply {
            setBackgroundColor(Ui.color(activity, R.color.menu_scrim))
            alpha = 0f
            setOnClickListener { dialog.dismiss() }
        }
        val column = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            clipChildren = false
            alpha = 0f
        }
        // The lifted row: the same content on a raised card, the size it had in the list.
        val card = FrameLayout(activity).apply {
            setBackgroundResource(R.drawable.card)
            elevation = Ui.dp(activity, 10f).toFloat()
            clipToOutline = true
            isClickable = true
            addView(preview, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        }
        column.addView(card, LinearLayout.LayoutParams(anchor.width, ViewGroup.LayoutParams.WRAP_CONTENT))
        val menu = menu(activity, actions) { dialog.dismiss() }
        column.addView(
            menu,
            LinearLayout.LayoutParams(Ui.dp(activity, 250f), ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                topMargin = Ui.dp(activity, 10f)
            },
        )
        root.addView(column, FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        dialog.setContentView(root)
        window.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)

        val content = activity.findViewById<View>(android.R.id.content)
        dialog.setOnDismissListener { if (Build.VERSION.SDK_INT >= 31) content.setRenderEffect(null) }
        if (Build.VERSION.SDK_INT >= 31) {
            val radius = Ui.dp(activity, 18f).toFloat()
            content.setRenderEffect(RenderEffect.createBlurEffect(radius, radius, Shader.TileMode.CLAMP))
        }

        val at = IntArray(2).also { anchor.getLocationOnScreen(it) }
        anchor.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
        dialog.show()
        root.post {
            val origin = IntArray(2).also { root.getLocationOnScreen(it) }
            val margin = Ui.dp(activity, 16f)
            val maxY = root.height - column.height - margin * 2
            column.x = (at[0] - origin[0]).toFloat()
            column.y = (at[1] - origin[1]).toFloat().coerceIn(margin * 2f, maxY.coerceAtLeast(margin * 2).toFloat())
            column.pivotX = Ui.dp(activity, 40f).toFloat()
            column.pivotY = card.height / 2f
            column.scaleX = 0.94f
            column.scaleY = 0.94f
            root.animate().alpha(1f).setDuration(160).start()
            column.animate().alpha(1f).scaleX(1f).scaleY(1f).setDuration(260).setInterpolator(OvershootInterpolator(1.4f)).start()
        }
    }

    private fun menu(activity: Activity, actions: List<Action>, close: () -> Unit): View {
        val menu = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundResource(R.drawable.menu_bg)
            elevation = Ui.dp(activity, 14f).toFloat()
            clipToOutline = true
            isClickable = true
        }
        actions.forEachIndexed { i, action ->
            if (i > 0) {
                // A thicker gap before the destructive action, as iOS groups it.
                val thick = action.destructive
                menu.addView(
                    View(activity).apply { setBackgroundColor(Ui.color(activity, if (thick) R.color.menu_gap else R.color.line)) },
                    LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ui.dp(activity, if (thick) 7f else 1f)),
                )
            }
            val tone = Ui.color(activity, if (action.destructive) R.color.accent_tx else R.color.text)
            val row = LayoutInflater.from(activity).inflate(R.layout.row_menu, menu, false)
            row.findViewById<TextView>(R.id.menu_label).apply {
                text = action.label
                setTextColor(tone)
                typeface = ResourcesCompat.getFont(activity, R.font.inter_regular)
                gravity = Gravity.CENTER_VERTICAL
            }
            row.findViewById<ImageView>(R.id.menu_icon).apply {
                setImageResource(action.icon)
                setColorFilter(tone)
            }
            row.setOnClickListener {
                close()
                action.run()
            }
            menu.addView(row)
        }
        return menu
    }
}
