package com.opendesk.app

import android.app.Application
import androidx.appcompat.app.AppCompatDelegate
import com.opendesk.app.data.settings.SettingsRepository

/** Applies the stored appearance (follow the phone, light or dark) before any screen is created. */
class OpenDeskApp : Application() {
    override fun onCreate() {
        super.onCreate()
        applyAppearance(SettingsRepository(this).appearance)
    }

    companion object {
        /** Switches every screen to `appearance` (`SettingsRepository.APPEARANCE_*`); open screens are recreated. */
        fun applyAppearance(appearance: Int) {
            AppCompatDelegate.setDefaultNightMode(
                when (appearance) {
                    SettingsRepository.APPEARANCE_LIGHT -> AppCompatDelegate.MODE_NIGHT_NO
                    SettingsRepository.APPEARANCE_DARK -> AppCompatDelegate.MODE_NIGHT_YES
                    else -> AppCompatDelegate.MODE_NIGHT_FOLLOW_SYSTEM
                },
            )
        }
    }
}
