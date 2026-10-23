package com.opendesk.app.session

import android.content.Context
import android.os.Environment
import android.util.Base64
import com.opendesk.app.R
import com.opendesk.app.data.dto.FilePayload
import java.io.File
import java.io.FileOutputStream
import java.security.MessageDigest

/**
 * Receives files sent from a PC into Downloads. Bytes go to a hidden `.part` file and are renamed only after
 * size and SHA-256 match; an existing file is never overwritten.
 *
 * SECURITY: every value comes from the remote peer. IDs, names, sizes and chunk lengths are validated before
 * anything touches the disk.
 */
class FileReceiver(private val context: Context, private val notify: (String) -> Unit) {

    private class Incoming(
        val output: FileOutputStream,
        val staged: File,
        val name: String,
        val size: Long,
        var received: Long = 0,
        val digest: MessageDigest = MessageDigest.getInstance("SHA-256"),
    )

    private val active = mutableMapOf<String, Incoming>()

    /** Handles one `start`, `chunk` or `end` payload from the PC. */
    fun handle(payload: FilePayload) {
        val id = payload.id ?: return
        if (id.length !in 1..MAX_ID_LENGTH || !id.all { (it.isLetterOrDigit() && it.code < 128) || it == '_' }) return
        when (payload.action) {
            "start" -> start(id, payload)
            "chunk" -> chunk(id, payload)
            "end" -> end(id, payload)
        }
    }

    /** Deletes every partial file. */
    fun close() {
        active.values.forEach {
            it.output.runCatching { close() }
            it.staged.delete()
        }
        active.clear()
    }

    private fun start(id: String, payload: FilePayload) {
        val size = payload.size ?: return
        val name = (payload.name ?: "opendesk_${System.currentTimeMillis()}.bin").replace(UNSAFE_NAME_CHARS, "_")
        val valid = active.size < MAX_ACTIVE && id !in active && size in 0..MAX_FILE_BYTES &&
            name.isNotBlank() && name.length <= MAX_NAME_LENGTH && name != "." && name != ".." && name.none { it.isISOControl() }
        if (!valid) return
        val staged = stagingFile() ?: return
        active[id] = Incoming(FileOutputStream(staged), staged, name, size)
        notify(context.getString(R.string.file_receiving, name))
    }

    /** Public Downloads, or the app's own download folder when it is not writable. */
    private fun stagingFile(): File? {
        val dirs = listOfNotNull(
            Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS),
            context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: context.filesDir,
        )
        for (dir in dirs) {
            runCatching {
                if (!dir.exists()) dir.mkdirs()
                return File.createTempFile(".opendesk-", ".part", dir)
            }
        }
        return null
    }

    private fun chunk(id: String, payload: FilePayload) {
        val incoming = active[id] ?: return
        val b64 = payload.chunk ?: return
        if (b64.length > MAX_CHUNK_B64) return
        val bytes = runCatching { Base64.decode(b64, Base64.DEFAULT) }.getOrNull() ?: return
        if (bytes.size > MAX_CHUNK_BYTES || incoming.received + bytes.size > incoming.size) return
        try {
            incoming.output.write(bytes)
            incoming.digest.update(bytes)
            incoming.received += bytes.size
        } catch (e: java.io.IOException) {
            discard(id)
        }
    }

    private fun end(id: String, payload: FilePayload) {
        val incoming = active.remove(id) ?: return
        val actual = incoming.digest.digest().joinToString("") { "%02x".format(it) }
        val target = File(incoming.staged.parentFile, incoming.name)
        val saved = try {
            incoming.output.flush()
            incoming.output.fd.sync()
            incoming.output.close()
            payload.sha256 == actual && incoming.received == incoming.size && !target.exists() && incoming.staged.renameTo(target)
        } catch (e: java.io.IOException) {
            false
        }
        if (saved) {
            notify(context.getString(R.string.file_saved, target.name))
        } else {
            incoming.staged.delete()
            notify(context.getString(R.string.file_failed))
        }
    }

    private fun discard(id: String) {
        active.remove(id)?.let {
            it.output.runCatching { close() }
            it.staged.delete()
        }
    }

    private companion object {
        const val MAX_ID_LENGTH = 80
        const val MAX_ACTIVE = 8
        const val MAX_FILE_BYTES = 2_147_483_648L
        const val MAX_NAME_LENGTH = 200
        const val MAX_CHUNK_BYTES = 32 * 1024
        const val MAX_CHUNK_B64 = 48_000
        val UNSAFE_NAME_CHARS = Regex("[\\\\/:*?\"<>|]")
    }
}
