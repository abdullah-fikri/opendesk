package com.opendesk.app.ui.monitor

import android.graphics.Color
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.google.android.material.bottomsheet.BottomSheetBehavior
import com.google.android.material.bottomsheet.BottomSheetDialog
import com.opendesk.app.R
import com.opendesk.app.domain.MonitorSettings
import com.opendesk.app.domain.MonitorWall
import com.opendesk.app.domain.WallHost
import com.opendesk.app.ui.common.Ui

/**
 * The wall's settings sheet: layout, automatic page rotation, and which computers are on the wall (every online
 * one, or a hand-picked list from the relay's registry). Every change applies at once through [onChange].
 */
class WallSettingsSheet(
    private val activity: AppCompatActivity,
    private var value: MonitorSettings,
    private val gridChoices: List<Int>,
    private val hosts: List<WallHost>,
    private val onChange: (MonitorSettings) -> Unit,
) {
    private val dialog = BottomSheetDialog(activity)
    private val body = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }

    fun show() {
        val content = ScrollView(activity).apply {
            setBackgroundResource(R.drawable.sheet_bg)
            addView(body)
        }
        val pad = Ui.dp(activity, 18f)
        body.setPadding(pad, Ui.dp(activity, 10f), pad, pad)
        ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
            v.setPadding(0, 0, 0, insets.getInsets(WindowInsetsCompat.Type.systemBars()).bottom)
            insets
        }
        build()
        dialog.setContentView(content)
        dialog.setOnShowListener {
            (content.parent as? View)?.setBackgroundColor(Color.TRANSPARENT)
            dialog.behavior.state = BottomSheetBehavior.STATE_EXPANDED
            dialog.behavior.skipCollapsed = true
        }
        dialog.show()
    }

    fun dismiss() = dialog.dismiss()

    private fun update(next: MonitorSettings) {
        value = next
        onChange(next)
        build()
    }

    private fun build() {
        body.removeAllViews()
        body.addView(
            View(activity).apply { setBackgroundResource(R.drawable.handle) },
            LinearLayout.LayoutParams(Ui.dp(activity, 36f), Ui.dp(activity, 4f)).apply { gravity = Gravity.CENTER_HORIZONTAL },
        )
        body.addView(TextView(activity, null, 0, R.style.Od_RowTitle).apply {
            setText(R.string.wall_settings)
            textSize = 18f
            setPadding(0, Ui.dp(activity, 14f), 0, 0)
        })

        body.addView(Ui.section(body, activity.getString(R.string.wall_layout)))
        body.addView(segments(gridChoices.map { "$it×$it" }, gridChoices.indexOf(MonitorWall.grid(value))) { i ->
            update(value.copy(grid = gridChoices[i]))
        })

        val card = Ui.card(activity).also { body.addView(it.apply { (layoutParams as ViewGroup.MarginLayoutParams).topMargin = Ui.dp(activity, 18f) }) }
        Ui.addRow(card, switchRow(card, R.drawable.tile_cyan, R.drawable.ic_history, activity.getString(R.string.wall_rotate), null, value.rotate) {
            update(value.copy(rotate = it))
        })
        if (value.rotate) {
            val choices = listOf(10, 30, 60)
            val row = LinearLayout(activity).apply {
                setPadding(Ui.dp(activity, 14f), Ui.dp(activity, 8f), Ui.dp(activity, 14f), Ui.dp(activity, 10f))
                addView(segments(choices.map { activity.getString(R.string.wall_secs, it) }, choices.indexOf(value.rotateSecs)) { i ->
                    update(value.copy(rotateSecs = choices[i]))
                })
            }
            Ui.addRow(card, row)
        }
        Ui.addRow(card, switchRow(card, R.drawable.tile_green, R.drawable.ic_desktop, activity.getString(R.string.wall_all), activity.getString(R.string.wall_all_sub), value.all) {
            update(value.copy(all = it))
        })

        if (!value.all && hosts.isNotEmpty()) {
            body.addView(Ui.section(body, activity.getString(R.string.wall_pick)))
            val picks = Ui.card(activity).also { body.addView(it) }
            hosts.sortedWith(compareBy({ it.name.lowercase() }, { it.id })).forEach { h ->
                val sub = activity.getString(if (h.online) R.string.presence_online else R.string.presence_offline)
                Ui.addRow(picks, switchRow(picks, R.drawable.tile_blue, R.drawable.ic_screen, h.name, sub, h.id in value.selected) {
                    update(value.copy(selected = MonitorWall.toggle(value.selected, h.id)))
                })
            }
        }
    }

    private fun switchRow(parent: ViewGroup, tile: Int, icon: Int, title: String, sub: String?, on: Boolean, onToggle: (Boolean) -> Unit): View {
        val row = Ui.inflate(parent, R.layout.row_setting)
        val switch = Ui.settingSwitch(row)
        Ui.settingRow(row, tile, icon, title, sub) { switch.toggle() }
        switch.isChecked = on
        switch.setOnCheckedChangeListener { _, checked -> if (checked != on) onToggle(checked) }
        return row
    }

    private fun segments(labels: List<String>, selected: Int, onPick: (Int) -> Unit): View {
        val track = LinearLayout(activity).apply {
            setBackgroundResource(R.drawable.seg_track)
            val p = Ui.dp(activity, 3f)
            setPadding(p, p, p, p)
        }
        labels.forEachIndexed { i, label ->
            val option = TextView(activity).apply {
                text = label
                gravity = Gravity.CENTER
                textSize = 14f
                setOnClickListener { if (i != selected) onPick(i) }
            }
            Ui.segment(option, i == selected)
            track.addView(option, LinearLayout.LayoutParams(0, Ui.dp(activity, 32f), 1f))
        }
        return track.apply { layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT) }
    }
}
