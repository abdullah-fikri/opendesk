package com.opendesk.app.data.security

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import com.opendesk.app.crypto.NoiseXX
import org.bouncycastle.util.encoders.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * This phone's end-to-end identity and the host keys it pinned, in app-private `SharedPreferences` (excluded from
 * backups and device transfer, see res/xml/data_extraction_rules.xml).
 *
 * - The X25519 private key is wrapped with an AES-GCM key that lives in the Android Keystore and never leaves it.
 *   A key that cannot be unwrapped (Keystore reset, restored app data) is replaced; hosts accept any viewer key.
 * - Pins map a host's clean desk ID to the key it presented first (trust on first use), as `known_hosts.json`
 *   does on the desktop.
 */
class DeviceKeys(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** Result of comparing a host's key with its pin. */
    sealed interface Pin {
        object New : Pin
        object Match : Pin
        class Mismatch(val pinned: ByteArray) : Pin
    }

    /** The private key, created on first use; null only when no key can be stored at all. */
    val privateKey: ByteArray? by lazy { load() ?: create() }

    /** The public half of [privateKey]. */
    val publicKey: ByteArray? get() = privateKey?.let(NoiseXX::publicKey)

    /** Compares `key` with the pin of `hostClean`; pins it when there was none. */
    fun check(hostClean: String, key: ByteArray): Pin {
        val pinned = pinned(hostClean) ?: run {
            prefs.edit().putString(PIN_PREFIX + hostClean, Base64.toBase64String(key)).apply()
            return Pin.New
        }
        return if (pinned.contentEquals(key)) Pin.Match else Pin.Mismatch(pinned)
    }

    /** The key pinned for `hostClean`, if any. */
    fun pinned(hostClean: String): ByteArray? =
        prefs.getString(PIN_PREFIX + hostClean, null)?.let { runCatching { Base64.decode(it) }.getOrNull() }

    /** Forgets the pin of `hostClean`, so the next session pins the key it presents. */
    fun forget(hostClean: String) {
        prefs.edit().remove(PIN_PREFIX + hostClean).apply()
    }

    private fun load(): ByteArray? {
        val stored = prefs.getString(KEY_IDENTITY, null) ?: return null
        val parts = stored.split(':')
        val key = runCatching {
            when (parts[0]) {
                "ks" -> unwrap(Base64.decode(parts[1]), Base64.decode(parts[2]))
                "plain" -> Base64.decode(parts[1])
                else -> null
            }
        }.getOrNull()
        return key?.takeIf { it.size == NoiseXX.DH_LEN }
    }

    private fun create(): ByteArray? {
        val key = NoiseXX.generatePrivateKey()
        val stored = runCatching {
            val (iv, sealed) = wrap(key)
            "ks:${Base64.toBase64String(iv)}:${Base64.toBase64String(sealed)}"
        }.getOrElse {
            // Some devices have a broken Keystore; the file is still private to this app.
            "plain:${Base64.toBase64String(key)}"
        }
        return if (prefs.edit().putString(KEY_IDENTITY, stored).commit()) key else null
    }

    private fun wrappingKey(): SecretKey {
        val store = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (store.getKey(WRAP_ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(WRAP_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    private fun wrap(plain: ByteArray): Pair<ByteArray, ByteArray> {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, wrappingKey())
        return cipher.iv to cipher.doFinal(plain)
    }

    private fun unwrap(iv: ByteArray, sealed: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, wrappingKey(), GCMParameterSpec(128, iv))
        return cipher.doFinal(sealed)
    }

    private companion object {
        const val PREFS_NAME = "opendesk_keys"
        const val KEY_IDENTITY = "identity"
        const val PIN_PREFIX = "pin_"
        const val KEYSTORE = "AndroidKeyStore"
        const val WRAP_ALIAS = "opendesk_identity_wrap"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
    }
}
