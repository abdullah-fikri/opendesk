package com.opendesk.app.data.history

import android.content.Context
import com.opendesk.app.domain.DeskId
import org.json.JSONArray
import org.json.JSONObject

/**
 * Sessions this phone opened, newest first, in app-private `SharedPreferences` (at most [MAX] entries). They fill
 * the Sessions tab, and outside administrator mode they are the Devices list ([knownDevices]).
 */
class SessionHistory(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** One session: the computer, when it started, how long it ran and whether it failed to open. */
    data class Record(
        val cleanId: String,
        val name: String,
        val platform: String,
        val startedAt: Long,
        val durationSec: Long,
        val failed: Boolean,
    )

    /** A computer this phone connected to, with its latest name. */
    data class Known(val cleanId: String, val name: String, val platform: String, val lastAt: Long)

    fun records(): List<Record> {
        val raw = prefs.getString(KEY_RECORDS, null) ?: return emptyList()
        val array = runCatching { JSONArray(raw) }.getOrNull() ?: return emptyList()
        return (0 until array.length()).mapNotNull { i ->
            val o = array.optJSONObject(i) ?: return@mapNotNull null
            val id = DeskId.clean(o.optString("id"))
            if (id.isEmpty()) return@mapNotNull null
            Record(id, o.optString("name"), o.optString("platform"), o.optLong("at"), o.optLong("dur"), o.optBoolean("failed"))
        }
    }

    /** Adds a session at the top. */
    fun add(record: Record) = save(listOf(record) + records())

    /** Sets the duration of the newest session with `cleanId` (it ended now). */
    fun finish(cleanId: String, endedAt: Long) {
        val list = records().toMutableList()
        val i = list.indexOfFirst { it.cleanId == cleanId && !it.failed }
        if (i < 0) return
        list[i] = list[i].copy(durationSec = ((endedAt - list[i].startedAt) / 1000).coerceAtLeast(0))
        save(list)
    }

    /** Forgets every session with `cleanId`. */
    fun remove(cleanId: String) = save(records().filter { it.cleanId != cleanId })

    /** Distinct computers, most recent first. */
    fun knownDevices(): List<Known> = records()
        .groupBy { it.cleanId }
        .map { (id, list) ->
            val newest = list.maxBy { it.startedAt }
            val named = list.filter { it.name.isNotEmpty() }.maxByOrNull { it.startedAt }
            Known(id, named?.name.orEmpty(), named?.platform.orEmpty(), newest.startedAt)
        }
        .sortedByDescending { it.lastAt }

    private fun save(list: List<Record>) {
        val array = JSONArray()
        list.take(MAX).forEach { r ->
            array.put(
                JSONObject()
                    .put("id", r.cleanId)
                    .put("name", r.name)
                    .put("platform", r.platform)
                    .put("at", r.startedAt)
                    .put("dur", r.durationSec)
                    .put("failed", r.failed),
            )
        }
        prefs.edit().putString(KEY_RECORDS, array.toString()).apply()
    }

    private companion object {
        const val PREFS_NAME = "opendesk_history"
        const val KEY_RECORDS = "records"
        const val MAX = 200
    }
}
