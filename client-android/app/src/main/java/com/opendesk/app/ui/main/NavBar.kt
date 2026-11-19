package com.opendesk.app.ui.main

import android.app.Activity
import android.view.View
import android.widget.ImageView
import android.widget.TextView
import androidx.core.content.res.ResourcesCompat
import com.opendesk.app.R
import com.opendesk.app.ui.common.Ui

/** The main screen's pages. Relay and Security open from Settings, and Back returns to [parent]. */
enum class Page(val parent: Page?) {
    DEVICES(null),
    SESSIONS(DEVICES),
    SETTINGS(DEVICES),
    RELAY(SETTINGS),
    SECURITY(SETTINGS),
    ;

    /** The tab that is highlighted while this page shows. */
    val tab: Page get() = if (this == RELAY || this == SECURITY) SETTINGS else this
}

/** The bottom bar (phone) or navigation rail (tablet): Devices, Sessions, Settings. */
class NavBar(private val activity: Activity, onSelect: (Page) -> Unit) {
    private val items = mapOf(
        Page.DEVICES to R.id.nav_devices,
        Page.SESSIONS to R.id.nav_sessions,
        Page.SETTINGS to R.id.nav_settings,
    )

    init {
        item(Page.DEVICES, R.drawable.ic_desktop, R.string.tab_devices, onSelect)
        item(Page.SESSIONS, R.drawable.ic_history, R.string.tab_sessions, onSelect)
        item(Page.SETTINGS, R.drawable.ic_gear, R.string.tab_settings, onSelect)
    }

    private fun item(page: Page, icon: Int, label: Int, onSelect: (Page) -> Unit) {
        val item = activity.findViewById<View>(items.getValue(page))
        item.findViewById<ImageView>(R.id.nav_icon).setImageResource(icon)
        item.findViewById<TextView>(R.id.nav_label).setText(label)
        item.setOnClickListener { onSelect(page) }
    }

    /** Highlights the tab of `page`. */
    fun select(page: Page) {
        for ((tab, id) in items) {
            val item = activity.findViewById<View>(id)
            val on = tab == page.tab
            val tone = Ui.color(activity, if (on) R.color.text else R.color.text3)
            item.findViewById<View>(R.id.nav_pill).setBackgroundResource(if (on) R.drawable.nav_pill else 0)
            item.findViewById<ImageView>(R.id.nav_icon).setColorFilter(tone)
            item.findViewById<TextView>(R.id.nav_label).apply {
                setTextColor(tone)
                typeface = ResourcesCompat.getFont(activity, if (on) R.font.inter_semibold else R.font.inter_regular)
            }
            item.isSelected = on
        }
    }
}
