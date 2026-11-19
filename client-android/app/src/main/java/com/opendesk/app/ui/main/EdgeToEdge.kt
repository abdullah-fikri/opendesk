package com.opendesk.app.ui.main

import android.app.Activity
import android.content.res.Configuration
import android.view.View
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.opendesk.app.R
import com.opendesk.app.ui.common.Ui

/**
 * The main screen edge to edge: the system bars draw over the app's own backgrounds (dark icons on the light
 * theme), and the content keeps clear of them and of the keyboard.
 */
internal object EdgeToEdge {
    fun apply(activity: Activity, isTablet: Boolean) {
        val window = activity.window
        WindowCompat.setDecorFitsSystemWindows(window, false)
        val night = (activity.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES
        WindowInsetsControllerCompat(window, window.decorView).apply {
            isAppearanceLightStatusBars = !night
            isAppearanceLightNavigationBars = !night
        }

        val root = activity.findViewById<View>(R.id.root_main)
        val content = activity.findViewById<View>(R.id.content)
        val nav = activity.findViewById<View>(R.id.nav_bar)
        val navTop = nav.paddingTop
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            val imeVisible = insets.isVisible(WindowInsetsCompat.Type.ime())
            root.setPadding(bars.left, 0, bars.right, 0)
            if (isTablet) {
                nav.setPadding(nav.paddingLeft, navTop + bars.top, nav.paddingRight, bars.bottom)
                content.setPadding(0, bars.top, 0, if (imeVisible) ime.bottom else bars.bottom)
            } else {
                content.setPadding(0, bars.top, 0, 0)
                // While typing the bottom bar steps aside and the page sits right above the keyboard.
                nav.visibility = if (imeVisible) View.GONE else View.VISIBLE
                root.setPadding(bars.left, 0, bars.right, if (imeVisible) ime.bottom else 0)
                nav.setPadding(nav.paddingLeft, navTop, nav.paddingRight, bars.bottom + Ui.dp(activity, 4f))
            }
            WindowInsetsCompat.CONSUMED
        }
    }
}
