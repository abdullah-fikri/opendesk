package com.opendesk.app.ui.devices

import android.app.Activity
import android.view.View
import android.widget.Toast
import com.opendesk.app.R
import com.opendesk.app.domain.DeviceItem
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Alerts
import com.opendesk.app.ui.main.MainHost

/**
 * What a long press on a computer offers: Connect, Rename and Delete. `onRenamed` / `onDeleted` run after the
 * change so the screen can refresh.
 */
class DeviceActions(
    private val activity: Activity,
    private val host: MainHost,
    private val onRenamed: (DeviceItem) -> Unit,
    private val onDeleted: (DeviceItem) -> Unit,
) {
    fun showMenu(anchor: View, item: DeviceItem, preview: View) {
        DeviceMenu.show(
            activity, anchor, preview,
            listOf(
                DeviceMenu.Action(activity.getString(R.string.menu_connect), R.drawable.ic_desktop) {
                    host.open(item)
                    host.connect(item)
                },
                DeviceMenu.Action(activity.getString(R.string.menu_rename), R.drawable.ic_pencil) { rename(item) },
                DeviceMenu.Action(activity.getString(R.string.menu_delete), R.drawable.ic_trash, destructive = true) { delete(item) },
            ),
        )
    }

    /** Admin list: the relay's alias, seen by every administrator. Otherwise a name kept on this phone. */
    private fun rename(item: DeviceItem) {
        val shared = host.directory.fromRelay
        Alerts.input(
            activity,
            activity.getString(R.string.rename_title),
            activity.getString(if (shared) R.string.rename_body_admin else R.string.rename_body_local),
            item.name.takeIf { it != activity.getString(R.string.unknown_computer) }.orEmpty(),
            activity.getString(R.string.rename_hint),
            activity.getString(R.string.save),
        ) { name ->
            if (shared) {
                // The relay answers with a new host-list (onHosts).
                if (!RelayConnection.renameHost(item.cleanId, name)) {
                    Toast.makeText(activity, R.string.relay_offline, Toast.LENGTH_SHORT).show()
                }
            } else {
                host.directory.setAlias(item.cleanId, name)
            }
            onRenamed(item)
        }
    }

    private fun delete(item: DeviceItem) {
        Alerts.show(
            activity,
            activity.getString(R.string.delete_title, item.name),
            activity.getString(R.string.delete_body),
            activity.getString(R.string.menu_delete),
            activity.getString(R.string.cancel),
            onPrimary = {
                host.directory.delete(item.cleanId)
                onDeleted(item)
            },
        )
    }
}
