package com.opendesk.app.ui.sessions

import android.content.Context
import android.text.format.DateFormat
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import com.opendesk.app.R
import com.opendesk.app.data.history.SessionHistory
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Ui
import com.opendesk.app.ui.main.MainHost
import java.util.Calendar
import java.util.Date
import java.util.Locale

/** Sessions tab: this phone's sessions grouped by day (Today, Yesterday, Earlier this week, Older). */
class SessionsPage(private val host: MainHost, parent: ViewGroup) {
    val view: View = LayoutInflater.from(parent.context).inflate(R.layout.page_sessions, parent, false)
    private val context = view.context
    private val list: LinearLayout = view.findViewById(R.id.sessions_list)
    private val empty: View = view.findViewById(R.id.sessions_empty)

    init {
        refresh()
    }

    fun refresh() {
        // Keep the title (first child).
        while (list.childCount > 1) list.removeViewAt(1)
        val records = host.history.records()
        empty.visibility = if (records.isEmpty()) View.VISIBLE else View.GONE
        if (records.isEmpty()) {
            empty.findViewById<ImageView>(R.id.empty_icon).setImageResource(R.drawable.ic_history)
            empty.findViewById<TextView>(R.id.empty_title).setText(R.string.sessions_empty_title)
            empty.findViewById<TextView>(R.id.empty_body).setText(R.string.sessions_empty_body)
            return
        }
        var group: String? = null
        var card: LinearLayout? = null
        for (record in records) {
            val label = groupLabel(context, record.startedAt)
            if (label != group || card == null) {
                group = label
                list.addView(Ui.section(list, label))
                card = Ui.card(context)
                list.addView(card)
            }
            Ui.addRow(card, row(card, record))
        }
    }

    private fun row(parent: ViewGroup, record: SessionHistory.Record): View {
        val row = Ui.inflate(parent, R.layout.row_session)
        val item = host.directory.lookup(record.cleanId)
        val name = host.directory.alias(record.cleanId) ?: record.name.ifEmpty { item.name }
        row.findViewById<TextView>(R.id.row_title).text = name
        row.findViewById<TextView>(R.id.row_sub).text = "${item.formattedId} · ${timeLabel(context, record.startedAt)}"
        val live = !record.failed && record.durationSec == 0L && RelayConnection.viewerPartner == record.cleanId
        val duration = row.findViewById<TextView>(R.id.row_duration)
        val state = row.findViewById<TextView>(R.id.row_state)
        when {
            record.failed -> {
                duration.text = "–"
                state.setText(R.string.session_failed)
                state.setTextColor(Ui.color(context, R.color.accent_tx))
            }
            live -> {
                duration.text = ""
                state.setText(R.string.session_live)
                state.setTextColor(Ui.color(context, R.color.green))
            }
            else -> {
                duration.text = Ui.duration(record.durationSec)
                state.setText(R.string.session_ended)
            }
        }
        row.setOnClickListener { host.open(item.copy(name = name)) }
        return row
    }

    companion object {
        private fun startOfDay(at: Long): Long = Calendar.getInstance().apply {
            timeInMillis = at
            set(Calendar.HOUR_OF_DAY, 0)
            set(Calendar.MINUTE, 0)
            set(Calendar.SECOND, 0)
            set(Calendar.MILLISECOND, 0)
        }.timeInMillis

        private fun daysAgo(at: Long): Long = (startOfDay(System.currentTimeMillis()) - startOfDay(at)) / 86_400_000L

        fun groupLabel(context: Context, at: Long): String = when (daysAgo(at)) {
            0L -> context.getString(R.string.day_today)
            1L -> context.getString(R.string.day_yesterday)
            in 2L..6L -> context.getString(R.string.day_this_week)
            else -> context.getString(R.string.day_older)
        }

        /** "today", "yesterday" or a date, lower-case for use inside a sentence. */
        fun dayLabel(context: Context, at: Long): String = when (daysAgo(at)) {
            0L -> context.getString(R.string.day_today).lowercase(Locale.getDefault())
            1L -> context.getString(R.string.day_yesterday).lowercase(Locale.getDefault())
            else -> DateFormat.format(DateFormat.getBestDateTimePattern(Locale.getDefault(), "dMMM"), Date(at)).toString()
        }

        /** "14:02" today and yesterday, "Mon 08:30" this week, "12 Nov 08:30" before. */
        fun timeLabel(context: Context, at: Long): String {
            val time = DateFormat.getTimeFormat(context).format(Date(at))
            return when (daysAgo(at)) {
                0L, 1L -> time
                in 2L..6L -> DateFormat.format("EEE", Date(at)).toString() + " " + time
                else -> DateFormat.format(DateFormat.getBestDateTimePattern(Locale.getDefault(), "dMMM"), Date(at)).toString() + " " + time
            }
        }
    }
}
