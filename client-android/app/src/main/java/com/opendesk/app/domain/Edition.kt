package com.opendesk.app.domain

import com.opendesk.app.BuildConfig

/**
 * Which edition this APK was built as (Gradle flavor `standard` or `admin`), like the desktop's `Edition`.
 *
 * The admin edition registers with `isAdmin`, lists the relay's device registry and has the monitor wall. The relay
 * still decides: with device credentials only its stored `admin` flag counts.
 */
object Edition {
    /** Built as the admin edition. */
    val isAdmin: Boolean = BuildConfig.ADMIN
}
