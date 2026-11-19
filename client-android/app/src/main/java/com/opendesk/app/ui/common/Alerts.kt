package com.opendesk.app.ui.common

import android.app.Activity
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatDialog
import com.opendesk.app.R

/** The redesign's centred alert (warning mark, title, message, a primary and a secondary button). */
object Alerts {
    fun show(
        activity: Activity,
        title: CharSequence,
        body: CharSequence,
        primary: CharSequence,
        secondary: CharSequence,
        onPrimary: () -> Unit,
        onSecondary: () -> Unit = {},
    ): AppCompatDialog? {
        if (activity.isFinishing || activity.isDestroyed) return null
        val dialog = AppCompatDialog(activity, R.style.Theme_OpenDesk_Dialog)
        val view = LayoutInflater.from(activity).inflate(R.layout.dialog_alert, null)
        view.findViewById<TextView>(R.id.alert_title).text = title
        view.findViewById<TextView>(R.id.alert_body).text = body
        view.findViewById<TextView>(R.id.alert_primary).apply {
            text = primary
            setOnClickListener {
                dialog.dismiss()
                onPrimary()
            }
        }
        view.findViewById<TextView>(R.id.alert_secondary).apply {
            text = secondary
            setOnClickListener {
                dialog.dismiss()
                onSecondary()
            }
        }
        dialog.setContentView(view)
        dialog.show()
        val width = minOf(activity.resources.displayMetrics.widthPixels - Ui.dp(activity, 48f), Ui.dp(activity, 360f))
        dialog.window?.setLayout(width, ViewGroup.LayoutParams.WRAP_CONTENT)
        return dialog
    }

    /** Asks for one line of text (renaming a device); `onSave` gets it trimmed. */
    fun input(
        activity: Activity,
        title: CharSequence,
        body: CharSequence,
        initial: String,
        hint: CharSequence,
        primary: CharSequence,
        onSave: (String) -> Unit,
    ): AppCompatDialog? {
        if (activity.isFinishing || activity.isDestroyed) return null
        val dialog = AppCompatDialog(activity, R.style.Theme_OpenDesk_Dialog)
        val view = LayoutInflater.from(activity).inflate(R.layout.dialog_alert, null)
        view.findViewById<View>(R.id.alert_mark).visibility = View.GONE
        view.findViewById<TextView>(R.id.alert_title).apply {
            text = title
            (layoutParams as ViewGroup.MarginLayoutParams).topMargin = 0
        }
        view.findViewById<TextView>(R.id.alert_body).text = body
        val field = view.findViewById<EditText>(R.id.alert_input).apply {
            visibility = View.VISIBLE
            setText(initial)
            setSelection(text.length)
            this.hint = hint
        }
        val save = {
            dialog.dismiss()
            onSave(field.text.toString().trim())
        }
        field.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == EditorInfo.IME_ACTION_DONE) save()
            actionId == EditorInfo.IME_ACTION_DONE
        }
        view.findViewById<TextView>(R.id.alert_primary).apply {
            text = primary
            setOnClickListener { save() }
        }
        view.findViewById<TextView>(R.id.alert_secondary).apply {
            setText(R.string.cancel)
            setOnClickListener { dialog.dismiss() }
        }
        dialog.setContentView(view)
        dialog.window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE or WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        dialog.show()
        val width = minOf(activity.resources.displayMetrics.widthPixels - Ui.dp(activity, 48f), Ui.dp(activity, 360f))
        dialog.window?.setLayout(width, ViewGroup.LayoutParams.WRAP_CONTENT)
        field.requestFocus()
        return dialog
    }

    /** A list of label / meaning pairs with one button (the touch-gesture guide). */
    fun table(activity: Activity, title: CharSequence, rows: List<Pair<String, String>>, button: CharSequence): AppCompatDialog? {
        if (activity.isFinishing || activity.isDestroyed) return null
        val dialog = AppCompatDialog(activity, R.style.Theme_OpenDesk_Dialog)
        val view = LayoutInflater.from(activity).inflate(R.layout.dialog_table, null)
        view.findViewById<TextView>(R.id.table_title).text = title
        val list = view.findViewById<ViewGroup>(R.id.table_rows)
        rows.forEach { (label, meaning) ->
            val row = LayoutInflater.from(activity).inflate(R.layout.row_table, list, false)
            row.findViewById<TextView>(R.id.table_label).text = label
            row.findViewById<TextView>(R.id.table_value).text = meaning
            list.addView(View(activity).apply { setBackgroundColor(Ui.color(activity, R.color.line)) }, ViewGroup.LayoutParams.MATCH_PARENT, Ui.dp(activity, 1f))
            list.addView(row)
        }
        view.findViewById<TextView>(R.id.table_button).apply {
            text = button
            setOnClickListener { dialog.dismiss() }
        }
        dialog.setContentView(view)
        dialog.show()
        val width = minOf(activity.resources.displayMetrics.widthPixels - Ui.dp(activity, 32f), Ui.dp(activity, 420f))
        dialog.window?.setLayout(width, ViewGroup.LayoutParams.WRAP_CONTENT)
        return dialog
    }
}
