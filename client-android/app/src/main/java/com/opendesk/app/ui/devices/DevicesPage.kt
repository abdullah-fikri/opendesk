package com.opendesk.app.ui.devices

import android.os.SystemClock
import android.text.Editable
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextWatcher
import android.text.style.ForegroundColorSpan
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import com.opendesk.app.R
import com.opendesk.app.domain.DeskId
import com.opendesk.app.domain.DeviceItem
import com.opendesk.app.domain.Edition
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Ui
import com.opendesk.app.ui.main.ConnectState
import com.opendesk.app.ui.main.MainHost
import com.opendesk.app.ui.sessions.SessionsPage

/**
 * Devices tab. Phone: list, search and the ID bar; a tap opens the connect sheet. Tablet: the same list beside a
 * detail pane for the selected computer, which also shows the connection attempt.
 */
class DevicesPage(private val host: MainHost, parent: ViewGroup) {
    val view: View = LayoutInflater.from(parent.context).inflate(R.layout.page_devices, parent, false)
    private val context = view.context

    private val sub: TextView = view.findViewById(R.id.devices_sub)
    private val search: EditText = view.findViewById(R.id.et_search)
    private val scroll: ScrollView = view.findViewById(R.id.devices_scroll)
    private val list: LinearLayout = view.findViewById(R.id.devices_list)
    private val empty: View = view.findViewById(R.id.devices_empty)
    private val idField: EditText = view.findViewById(R.id.et_target_id)
    private val idConnect: TextView = view.findViewById(R.id.btn_connect)
    private val chip: View? = view.findViewById(R.id.relay_chip)

    // Tablet only.
    private val detail: View? = view.findViewById(R.id.detail)
    private var selectedId: String? = null

    init {
        search.addTextChangedListener(watcher { refresh() })
        idField.addTextChangedListener(watcher { idConnect.isEnabled = DeskId.clean(it).isNotEmpty() })
        idConnect.isEnabled = false
        view.findViewById<View>(R.id.btn_monitor_wall).apply {
            visibility = if (Edition.isAdmin) View.VISIBLE else View.GONE
            setOnClickListener { host.showMonitorWall() }
        }
        idConnect.setOnClickListener { connectTyped() }
        idField.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == EditorInfo.IME_ACTION_GO) {
                connectTyped()
                true
            } else {
                false
            }
        }
        refresh()
    }

    private fun connectTyped() {
        val id = DeskId.clean(idField.text.toString())
        if (id.isEmpty()) return
        val item = host.directory.lookup(id)
        if (host.isTablet) select(item.cleanId)
        host.open(item)
        host.connect(item)
    }

    /** Re-reads the relay state, the list and (tablet) the detail pane. */
    fun refresh() {
        updateChip()
        val query = search.text.toString().trim()
        val loading = host.directory.loading(SystemClock.uptimeMillis() - host.onlineSinceMs)
        val all = if (loading) emptyList() else host.directory.all()
        val shown = all.filter { it.matches(query) }
        val fromRelay = host.directory.fromRelay

        sub.text = when {
            query.isNotEmpty() -> context.resources.getQuantityString(R.plurals.search_results, shown.size, shown.size, query)
            all.isEmpty() && !loading -> context.getString(R.string.devices_none)
            fromRelay -> Ui.dotted(
                context,
                context.getString(R.string.devices_online, all.count { it.online == true }) to R.color.green,
                context.getString(R.string.devices_offline, all.count { it.online != true }) to R.color.offline,
            )
            loading -> context.getString(R.string.devices_loading)
            else -> context.getString(R.string.devices_recent, all.size)
        }

        list.removeAllViews()
        when {
            loading -> {
                showEmpty(null)
                list.addView(Ui.section(list, context.getString(R.string.devices_loading)))
                val card = Ui.card(context)
                repeat(SKELETON_ROWS) { Ui.addRow(card, skeletonRow()) }
                list.addView(card)
            }
            shown.isEmpty() && query.isNotEmpty() -> showEmpty(
                Triple(R.drawable.ic_search, context.getString(R.string.no_results_title), context.getString(R.string.no_results_body, query)),
            )
            shown.isEmpty() -> showEmpty(
                Triple(
                    R.drawable.ic_desktop,
                    context.getString(R.string.devices_none),
                    context.getString(if (Edition.isAdmin) R.string.devices_empty_admin else R.string.devices_empty_recent),
                ),
            )
            else -> {
                showEmpty(null)
                if (fromRelay) {
                    addSection(R.string.section_online, shown.filter { it.online == true })
                    addSection(R.string.section_offline, shown.filter { it.online != true })
                } else {
                    addSection(R.string.section_recent, shown)
                }
            }
        }
        refreshDetail()
    }

    private fun addSection(title: Int, items: List<DeviceItem>) {
        if (items.isEmpty()) return
        list.addView(Ui.section(list, context.getString(title, items.size)))
        val card = Ui.card(context)
        items.forEach { Ui.addRow(card, deviceRow(card, it)) }
        list.addView(card)
    }

    private fun deviceRow(parent: ViewGroup, item: DeviceItem): View {
        val row = Ui.inflate(parent, R.layout.row_device)
        row.findViewById<TextView>(R.id.row_title).text = item.name
        row.findViewById<TextView>(R.id.row_sub).text = item.formattedId
        row.findViewById<View>(R.id.row_dot).apply {
            when (item.online) {
                true -> setBackgroundResource(R.drawable.status_badge)
                false -> setBackgroundResource(R.drawable.status_badge_off)
                null -> visibility = View.GONE
            }
        }
        if (host.isTablet && item.cleanId == selectedId) row.setBackgroundResource(R.drawable.row_selected)
        row.setOnLongClickListener {
            host.showMenu(row, item, preview(parent, item))
            true
        }
        row.setOnClickListener {
            if (host.isTablet) select(item.cleanId)
            host.open(item)
        }
        row.findViewById<View>(R.id.row_action).setOnClickListener {
            if (host.isTablet) select(item.cleanId)
            host.open(item)
            host.connect(item)
        }
        return row
    }

    /** The row as the long-press menu lifts it: same content, no actions of its own. */
    private fun preview(parent: ViewGroup, item: DeviceItem): View {
        val row = Ui.inflate(parent, R.layout.row_device)
        row.background = null
        row.findViewById<TextView>(R.id.row_title).text = item.name
        row.findViewById<TextView>(R.id.row_sub).text = item.formattedId
        row.findViewById<View>(R.id.row_dot).apply {
            when (item.online) {
                true -> setBackgroundResource(R.drawable.status_badge)
                false -> setBackgroundResource(R.drawable.status_badge_off)
                null -> visibility = View.GONE
            }
        }
        row.findViewById<View>(R.id.row_action).isClickable = false
        return row
    }

    private fun skeletonRow(): View {
        val c = context
        return LinearLayout(c).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = android.view.Gravity.CENTER_VERTICAL
            minimumHeight = Ui.dp(c, 58f)
            setPadding(Ui.dp(c, 14f), Ui.dp(c, 9f), Ui.dp(c, 14f), Ui.dp(c, 9f))
            addView(View(c).apply { setBackgroundResource(R.drawable.skeleton) }, LinearLayout.LayoutParams(Ui.dp(c, 38f), Ui.dp(c, 38f)))
            addView(
                LinearLayout(c).apply {
                    orientation = LinearLayout.VERTICAL
                    addView(View(c).apply { setBackgroundResource(R.drawable.skeleton) }, LinearLayout.LayoutParams(Ui.dp(c, 150f), Ui.dp(c, 11f)))
                    addView(
                        View(c).apply { setBackgroundResource(R.drawable.skeleton) },
                        LinearLayout.LayoutParams(Ui.dp(c, 90f), Ui.dp(c, 8f)).apply { topMargin = Ui.dp(c, 8f) },
                    )
                },
                LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { marginStart = Ui.dp(c, 12f) },
            )
        }
    }

    private fun showEmpty(content: Triple<Int, String, String>?) {
        scroll.visibility = if (content == null) View.VISIBLE else View.GONE
        empty.visibility = if (content == null) View.GONE else View.VISIBLE
        content ?: return
        empty.findViewById<ImageView>(R.id.empty_icon).setImageResource(content.first)
        empty.findViewById<TextView>(R.id.empty_title).text = content.second
        empty.findViewById<TextView>(R.id.empty_body).text = content.third
    }

    /**
     * The relay chip shows only when something needs the user: no relay set, or the relay unreachable. While
     * connected (or still connecting) the header stays clean.
     */
    private fun updateChip() {
        val chip = chip ?: return
        val label = when {
            RelayConnection.online -> null
            host.settings.gatewayUrl.isEmpty() -> R.string.relay_chip_none
            RelayConnection.nextRetryAtMs > 0 -> R.string.relay_chip_offline
            else -> null
        }
        chip.visibility = if (label == null) View.GONE else View.VISIBLE
        label ?: return
        chip.findViewById<TextView>(R.id.relay_chip_text).apply {
            setText(label)
            setTextColor(Ui.color(context, R.color.text2))
        }
        chip.findViewById<View>(R.id.relay_chip_dot).backgroundTintList =
            android.content.res.ColorStateList.valueOf(Ui.color(context, R.color.offline))
        chip.setBackgroundResource(R.drawable.chip_neutral)
        chip.setOnClickListener { host.showRelay() }
    }

    // ---- Tablet detail pane ----

    /** Tablet: shows `cleanId` in the detail pane. */
    fun select(cleanId: String) {
        if (selectedId == cleanId) return
        selectedId = cleanId
        refresh()
    }

    private fun refreshDetail() {
        val detail = detail ?: return
        val id = selectedId
        val item = id?.let { host.directory.lookup(it) }
        val content = detail.findViewById<View>(R.id.detail_content)
        val emptyPane = detail.findViewById<View>(R.id.detail_empty)
        if (item == null) {
            content.visibility = View.GONE
            emptyPane.visibility = View.VISIBLE
            emptyPane.findViewById<ImageView>(R.id.empty_icon).setImageResource(R.drawable.ic_desktop)
            emptyPane.findViewById<TextView>(R.id.empty_title).setText(R.string.detail_pick_title)
            emptyPane.findViewById<TextView>(R.id.empty_body).setText(R.string.detail_pick_body)
            return
        }
        content.visibility = View.VISIBLE
        emptyPane.visibility = View.GONE
        detail.findViewById<TextView>(R.id.detail_name).text = item.name
        detail.findViewById<TextView>(R.id.detail_id).text =
            if (item.platform.isEmpty()) item.formattedId else context.getString(R.string.id_platform, item.formattedId, item.platform)
        detail.findViewById<TextView>(R.id.detail_status).text = statusLine(item)

        val state = if (host.target?.cleanId == item.cleanId) host.connectState else ConnectState.Ready
        val stateView = detail.findViewById<View>(R.id.detail_state)
        ConnectStateView.bind(stateView, item, state, showReady = false)
        detail.findViewById<TextView>(R.id.detail_connect).apply {
            setText(if (state is ConnectState.Failed) R.string.try_again else R.string.connect_to_pc)
            isEnabled = state != ConnectState.Connecting
            setOnClickListener { host.connect(item) }
        }

        val last = host.history.records().firstOrNull { it.cleanId == item.cleanId && !it.failed }
        Ui.settingRow(
            detail.findViewById(R.id.detail_recent), R.drawable.tile_cyan, R.drawable.ic_history,
            context.getString(R.string.recent_sessions),
            last?.let { "${Ui.duration(it.durationSec)} · ${SessionsPage.dayLabel(context, it.startedAt)}" }
                ?: context.getString(R.string.recent_none),
            chevron = true,
        ) { host.showSessions() }
        val pinned = item.cleanId in RelayConnection.pinnedHosts()
        Ui.settingRow(
            detail.findViewById(R.id.detail_security), R.drawable.tile_green, R.drawable.ic_lock,
            context.getString(R.string.security),
            context.getString(if (pinned) R.string.key_pinned else R.string.key_pinned_first),
            chevron = true,
        ) { host.showSecurity() }
    }

    /** "● Online · End-to-end encrypted · viewer only". */
    private fun statusLine(item: DeviceItem): CharSequence {
        val out = SpannableStringBuilder()
        if (item.online != null) {
            val tone = if (item.online) R.color.green else R.color.offline
            val start = out.length
            out.append("● ").append(context.getString(if (item.online) R.string.presence_online else R.string.presence_offline))
            out.setSpan(ForegroundColorSpan(Ui.color(context, tone)), start, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            out.append("  ·  ")
        }
        out.append(context.getString(R.string.e2e_viewer_only))
        return out
    }

    private fun watcher(onChange: (String) -> Unit) = object : TextWatcher {
        override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) = Unit
        override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) = Unit
        override fun afterTextChanged(s: Editable?) = onChange(s?.toString().orEmpty())
    }

    private companion object {
        const val SKELETON_ROWS = 7
    }
}
