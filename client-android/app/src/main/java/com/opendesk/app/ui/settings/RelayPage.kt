package com.opendesk.app.ui.settings

import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.Editable
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextWatcher
import android.text.style.TypefaceSpan
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.EditText
import android.widget.TextView
import com.opendesk.app.R
import com.opendesk.app.domain.GatewayUrl
import com.opendesk.app.session.RelayConnection
import com.opendesk.app.ui.common.Ui
import com.opendesk.app.ui.main.MainHost
import kotlin.math.ceil

/**
 * Relay settings: the address (checked as it is typed: plain ws:// only to this device) and an optional device
 * token (at least 32 characters). Shows whether the relay is connected, or when it retries.
 */
class RelayPage(private val host: MainHost, parent: ViewGroup) {
    val view: View = LayoutInflater.from(parent.context).inflate(R.layout.page_relay, parent, false)
    private val context = view.context

    private val address: EditText = view.findViewById(R.id.et_relay)
    private val token: EditText = view.findViewById(R.id.et_token)
    private val addressError: TextView = view.findViewById(R.id.relay_error)
    private val tokenError: TextView = view.findViewById(R.id.token_error)
    private val savedAs: TextView = view.findViewById(R.id.relay_saved_as)
    private val save: TextView = view.findViewById(R.id.btn_save_relay)
    private val status: View = view.findViewById(R.id.relay_status)
    private val banner: View = view.findViewById(R.id.relay_banner)

    private val storedToken = host.settings.tokenForGateway
    private var justSaved = false
    private val ticker = Handler(Looper.getMainLooper())
    private val tick = object : Runnable {
        override fun run() {
            refresh()
            if (view.isAttachedToWindow) ticker.postDelayed(this, 1000L)
        }
    }

    init {
        view.findViewById<View>(R.id.page_back).setOnClickListener { host.showSettings() }
        val url = host.settings.gatewayUrl
        address.setText(if (url.isEmpty()) "" else SettingsPage.displayUrl(url))
        token.setText(storedToken)
        val watcher = object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) = Unit
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) = Unit
            override fun afterTextChanged(s: Editable?) {
                validate()
            }
        }
        address.addTextChangedListener(watcher)
        token.addTextChangedListener(watcher)
        save.setOnClickListener { saveAndConnect() }
        view.addOnAttachStateChangeListener(object : View.OnAttachStateChangeListener {
            override fun onViewAttachedToWindow(v: View) {
                ticker.removeCallbacks(tick)
                ticker.post(tick)
            }

            override fun onViewDetachedFromWindow(v: View) = ticker.removeCallbacks(tick)
        })
        validate()
        refresh()
    }

    /** The normalized address, or empty. */
    private fun normalized(): String = GatewayUrl.normalize(address.text.toString())

    private fun validate(): Boolean {
        val url = normalized()
        val urlOk = url.isEmpty() || SettingsPage.permitted(url)
        addressError.visibility = if (urlOk) View.GONE else View.VISIBLE
        addressError.setText(R.string.relay_plaintext_refused)
        address.setBackgroundResource(if (urlOk) R.drawable.field else R.drawable.field_error)
        address.typeface = if (urlOk) null else Ui.mono(context)

        val tokenText = token.text.toString().trim()
        val tokenOk = tokenText.isEmpty() || tokenText == storedToken || tokenText.length >= MIN_TOKEN
        tokenError.visibility = if (tokenOk) View.GONE else View.VISIBLE
        token.setBackgroundResource(if (tokenOk) R.drawable.field else R.drawable.field_error)
        // A rejected token is shown in clear so the user sees what is wrong with it.
        val inputType = android.text.InputType.TYPE_CLASS_TEXT or if (tokenOk) {
            android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD
        } else {
            android.text.InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
        }
        if (token.inputType != inputType) {
            token.inputType = inputType
            token.typeface = Ui.mono(context)
            token.setSelection(token.text.length)
        }

        val ok = url.isNotEmpty() && urlOk && tokenOk
        save.isEnabled = ok
        return ok
    }

    private fun saveAndConnect() {
        if (!validate()) return
        val settings = host.settings
        settings.gatewayUrl = normalized()
        val tokenText = token.text.toString().trim()
        if (tokenText != storedToken) {
            // Set by hand: every relay receives it (see SettingsRepository.tokenForGateway).
            settings.deviceToken = tokenText
            settings.deviceTokenRelay = ""
        }
        justSaved = true
        host.restartRelay()
        refresh()
    }

    /** Status line, unreachable banner and "Saved as". */
    fun refresh() {
        val url = host.settings.gatewayUrl
        savedAs.visibility = if (url.isEmpty()) View.GONE else View.VISIBLE
        if (url.isNotEmpty()) {
            val text = SpannableStringBuilder(context.getString(R.string.saved_as, url))
            val start = text.indexOf(url)
            if (start >= 0) text.setSpan(TypefaceSpan("monospace"), start, start + url.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            savedAs.text = text
        }
        val online = RelayConnection.online
        status.visibility = if (online && url.isNotEmpty()) View.VISIBLE else View.GONE
        view.findViewById<TextView>(R.id.relay_status_text).setText(
            if (justSaved) R.string.relay_updated_connected else R.string.status_connected,
        )
        val retryAt = RelayConnection.nextRetryAtMs
        val failing = !online && url.isNotEmpty() && retryAt > 0
        banner.visibility = if (failing) View.VISIBLE else View.GONE
        if (failing) {
            val seconds = ceil((retryAt - SystemClock.uptimeMillis()) / 1000.0).toInt()
            view.findViewById<TextView>(R.id.relay_banner_body).text =
                if (seconds > 0) context.getString(R.string.relay_retry_in, seconds) else context.getString(R.string.relay_retrying)
        }
    }

    private companion object {
        const val MIN_TOKEN = 32
    }
}
