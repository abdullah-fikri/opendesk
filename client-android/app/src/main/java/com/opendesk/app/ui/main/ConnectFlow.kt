package com.opendesk.app.ui.main

import android.content.Context
import android.os.Handler
import android.os.Looper
import com.opendesk.app.R
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.DeviceItem
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.devices.DeviceDirectory

/**
 * One connection attempt at a time: which computer it is for ([target]), where it stands ([state]), the timeout for
 * a request the relay never answers, and the Sessions entry it leaves. `onChange` runs after every state change.
 */
class ConnectFlow(
    private val context: Context,
    private val history: SessionHistory,
    private val directory: DeviceDirectory,
    private val onChange: () -> Unit,
) {
    private val handler = Handler(Looper.getMainLooper())
    private val timeout = Runnable { fail(context.getString(R.string.connect_failed_body)) }

    /** The computer the current (or last) attempt is for. */
    var target: DeviceItem? = null
        private set
    var state: ConnectState = ConnectState.Ready
        private set
    val connecting: Boolean get() = state == ConnectState.Connecting

    /** `item` is shown (sheet or detail pane); a pending attempt for another computer keeps its target. */
    fun focus(item: DeviceItem) {
        if (target?.cleanId != item.cleanId && !connecting) {
            target = item
            state = ConnectState.Ready
        }
    }

    /** Sends the connect request; with the relay offline the attempt fails at once. */
    fun start(item: DeviceItem) {
        target = item
        if (!RelayConnection.connectTo(item.cleanId)) {
            fail(context.getString(R.string.relay_offline), record = false)
            return
        }
        state = ConnectState.Connecting
        handler.removeCallbacks(timeout)
        handler.postDelayed(timeout, CONNECT_TIMEOUT_MS)
        onChange()
    }

    /** Abandons a pending attempt (the relay may still answer; the answer is then ignored). */
    fun cancel() {
        if (!connecting) return
        handler.removeCallbacks(timeout)
        state = ConnectState.Ready
        onChange()
    }

    /** The attempt failed; `record` adds a Failed entry to Sessions. */
    fun fail(message: String, record: Boolean = true) {
        handler.removeCallbacks(timeout)
        val item = target
        if (record && item != null) {
            history.add(SessionHistory.Record(item.cleanId, item.name, item.platform, System.currentTimeMillis(), 0, failed = true))
        }
        state = ConnectState.Failed(message)
        onChange()
    }

    /** The session started: records it in Sessions and returns the computer. */
    fun connected(partnerClean: String): DeviceItem {
        handler.removeCallbacks(timeout)
        val clean = DeskId.clean(partnerClean)
        val item = target?.takeIf { it.cleanId == clean } ?: directory.lookup(clean)
        val unknown = context.getString(R.string.unknown_computer)
        history.add(
            SessionHistory.Record(clean, item.name.takeIf { it != unknown }.orEmpty(), item.platform, System.currentTimeMillis(), 0, failed = false),
        )
        target = item
        state = ConnectState.Ready
        return item
    }

    /** Ends a pending attempt without recording it (a security alert takes over). */
    fun reset() {
        handler.removeCallbacks(timeout)
        state = ConnectState.Ready
    }

    /** A computer was renamed or deleted: the target follows. */
    fun renamed(cleanId: String) {
        if (target?.cleanId == cleanId) target = directory.lookup(cleanId)
    }

    fun deleted(cleanId: String) {
        if (target?.cleanId == cleanId && !connecting) target = null
    }

    fun dispose() = handler.removeCallbacksAndMessages(null)

    private companion object {
        /** A connect request the relay never answers (lost message, dead host) fails after this. */
        const val CONNECT_TIMEOUT_MS = 20_000L
    }
}
