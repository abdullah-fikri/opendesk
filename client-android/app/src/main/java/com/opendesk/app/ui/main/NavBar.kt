package com.opendesk.app.ui.main

import android.app.Activity
import android.view.HapticFeedbackConstants
import android.view.View
import android.widget.ImageView
import android.widget.TextView
import androidx.core.content.res.ResourcesCompat
import com.opendesk.app.R
import com.opendesk.app.domain.Edition
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

/**
 * The bottom bar (phone) or navigation rail (tablet), iOS tab bar style: Devices, Sessions, Wall (admin edition;
 * opens the monitor wall instead of a page) and Settings. The selected tab shows its filled icon in the accent
 * colour, the others their outlined icon in grey.
 */
class NavBar(private val activity: Activity, onSelect: (Page) -> Unit, onWall: () -> Unit) {
    private class Tab(val id: Int, val icon: Int, val iconOn: Int)

    private val items = mapOf(
        Page.DEVICES to Tab(R.id.nav_devices, R.drawable.ic_tab_devices, R.drawable.ic_tab_devices_on),
        Page.SESSIONS to Tab(R.id.nav_sessions, R.drawable.ic_tab_sessions, R.drawable.ic_tab_sessions_on),
        Page.SETTINGS to Tab(R.id.nav_settings, R.drawable.ic_tab_settings, R.drawable.ic_tab_settings_on),
    )

    init {
        item(R.id.nav_devices, R.drawable.ic_tab_devices, R.string.tab_devices) { onSelect(Page.DEVICES) }
        item(R.id.nav_sessions, R.drawable.ic_tab_sessions, R.string.tab_sessions) { onSelect(Page.SESSIONS) }
        item(R.id.nav_settings, R.drawable.ic_tab_settings, R.string.tab_settings) { onSelect(Page.SETTINGS) }
        activity.findViewById<View>(R.id.nav_wall)?.let { wall ->
            wall.visibility = if (Edition.isAdmin) View.VISIBLE else View.GONE
            item(R.id.nav_wall, R.drawable.ic_tab_wall, R.string.tab_wall) { onWall() }
            tint(wall, false)
        }
    }

    private fun item(id: Int, icon: Int, label: Int, onClick: () -> Unit) {
        val item = activity.findViewById<View>(id)
        item.findViewById<ImageView>(R.id.nav_icon).setImageResource(icon)
        item.findViewById<TextView>(R.id.nav_label).setText(label)
        item.setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.CLOCK_TICK)
            onClick()
        }
    }

    /** Highlights the tab of `page`. */
    fun select(page: Page) {
        for ((tab, t) in items) {
            val item = activity.findViewById<View>(t.id)
            val on = tab == page.tab
            item.findViewById<ImageView>(R.id.nav_icon).setImageResource(if (on) t.iconOn else t.icon)
            tint(item, on)
            item.isSelected = on
        }
    }

    private fun tint(item: View, on: Boolean) {
        val tone = Ui.color(activity, if (on) R.color.accent else R.color.text3)
        item.findViewById<ImageView>(R.id.nav_icon).setColorFilter(tone)
        item.findViewById<TextView>(R.id.nav_label).apply {
            setTextColor(tone)
            typeface = ResourcesCompat.getFont(activity, R.font.inter_medium)
        }
    }
}
