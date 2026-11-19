package com.opendesk.app.ui.devices

import android.content.Context
import com.opendesk.app.R
import com.opendesk.app.data.devices.LocalDeviceStore
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.data.settings.SettingsRepository
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.DeviceItem
import com.opendesk.app.domain.Edition
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Ui

/**
 * Where the Devices list comes from. In the admin edition it is the relay's registry (`host-list`, live presence);
 * otherwise, or until the relay sends one, the computers this phone connected to.
 *
 * Names: in the registry an administrator's alias (set with [RelayConnection.renameHost]) wins; elsewhere a name
 * given on this phone ([setAlias]). Deleted computers leave the history and stay hidden from the registry list on
 * this phone until it connects to them again.
 */
class DeviceDirectory(private val context: Context, private val settings: SettingsRepository, private val history: SessionHistory) {
    private val local = LocalDeviceStore(context)

    /** The admin edition, and the relay answered with its registry. */
    val fromRelay: Boolean get() = Edition.isAdmin && RelayConnection.hosts != null

    /** The admin edition waits for the registry on a live connection. */
    fun loading(sinceOnlineMs: Long): Boolean =
        Edition.isAdmin && RelayConnection.online && RelayConnection.hosts == null && sinceOnlineMs < REGISTRY_WAIT_MS

    fun all(): List<DeviceItem> {
        val own = DeskId.clean(settings.deskId)
        val hidden = local.hidden()
        val hosts = RelayConnection.hosts
        if (Edition.isAdmin && hosts != null) {
            return hosts
                .map { h -> h to DeskId.clean(h.cleanId.orEmpty()) }
                .filter { (h, id) -> id != own && id !in hidden && h.platform?.startsWith("android") != true }
                .map { (h, id) ->
                    val name = h.alias.orEmpty().ifEmpty { alias(id).orEmpty() }.ifEmpty { h.deviceName.orEmpty() }
                        .ifEmpty { context.getString(R.string.unknown_computer) }
                    DeviceItem(id, name, Ui.platformLabel(h.platform), h.online)
                }
                .sortedWith(compareBy({ it.online != true }, { it.name.lowercase() }))
        }
        return history.knownDevices().filter { it.cleanId != own }.map { k ->
            DeviceItem(k.cleanId, alias(k.cleanId) ?: k.name.ifEmpty { context.getString(R.string.unknown_computer) }, k.platform, null)
        }
    }

    /** The best description of `cleanId` this phone has (for an ID typed by hand). */
    fun lookup(cleanId: String): DeviceItem {
        val id = DeskId.clean(cleanId)
        all().firstOrNull { it.cleanId == id }?.let { return it }
        RelayConnection.hosts?.firstOrNull { DeskId.clean(it.cleanId.orEmpty()) == id }?.let { h ->
            val name = h.alias.orEmpty().ifEmpty { alias(id).orEmpty() }.ifEmpty { h.deviceName.orEmpty() }
            if (name.isNotEmpty()) return DeviceItem(id, name, Ui.platformLabel(h.platform), h.online)
        }
        alias(id)?.let { return DeviceItem(id, it, "", null) }
        history.knownDevices().firstOrNull { it.cleanId == id && it.name.isNotEmpty() }?.let {
            return DeviceItem(id, it.name, it.platform, null)
        }
        return DeviceItem(id, context.getString(R.string.unknown_computer), "", null)
    }

    /** The name given to `cleanId` on this phone, if any. */
    fun alias(cleanId: String): String? = local.alias(cleanId)

    /** Names `cleanId` on this phone; an empty name clears it. */
    fun setAlias(cleanId: String, name: String) = local.setAlias(cleanId, name)

    /** Removes `cleanId` from this phone's list: its sessions, its name, and from the registry list. */
    fun delete(cleanId: String) {
        history.remove(DeskId.clean(cleanId))
        local.hide(cleanId)
    }

    /** Shows `cleanId` again (this phone connects to it). */
    fun unhide(cleanId: String) = local.unhide(cleanId)

    companion object {
        /** How long the admin edition shows the loading list before falling back to recent computers. */
        const val REGISTRY_WAIT_MS = 4_000L
    }
}
