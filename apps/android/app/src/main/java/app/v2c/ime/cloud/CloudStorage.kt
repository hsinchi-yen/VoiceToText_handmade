package app.v2c.ime.cloud

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Minimal string key/value storage (SharedPreferences in production, a map in tests). */
interface KeyValueStore {
    fun get(key: String): String?
    fun put(key: String, value: String)
    fun remove(key: String)
}

class InMemoryKeyValueStore : KeyValueStore {
    val map = LinkedHashMap<String, String>()
    override fun get(key: String): String? = map[key]
    override fun put(key: String, value: String) {
        map[key] = value
    }

    override fun remove(key: String) {
        map.remove(key)
    }
}

/** [KeyValueStore] in an app-private preferences file. Android glue. */
class SharedPrefsKeyValueStore(context: Context, file: String) : KeyValueStore {
    private val prefs = context.getSharedPreferences(file, Context.MODE_PRIVATE)
    override fun get(key: String): String? = prefs.getString(key, null)
    override fun put(key: String, value: String) {
        prefs.edit().putString(key, value).apply()
    }

    override fun remove(key: String) {
        prefs.edit().remove(key).apply()
    }
}

/**
 * Encrypts secrets for storage. [encrypt] returns an opaque text token; [decrypt] returns null
 * when the token cannot be read (key invalidated, corrupted, restored on another device).
 */
interface SecretCipher {
    fun encrypt(plain: String): String
    fun decrypt(token: String): String?
}

/**
 * AES-256-GCM with a key held in the Android Keystore (never exported). Token layout:
 * Base64(iv[12] + ciphertext + tag). Android glue, not unit tested (needs a device).
 */
class KeystoreSecretCipher(private val alias: String = DEFAULT_ALIAS) : SecretCipher {

    override fun encrypt(plain: String): String {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, secretKey())
        val body = cipher.doFinal(plain.toByteArray(Charsets.UTF_8))
        return Base64.getEncoder().encodeToString(cipher.iv + body)
    }

    override fun decrypt(token: String): String? = try {
        val raw = Base64.getDecoder().decode(token)
        if (raw.size <= IV_BYTES) {
            null
        } else {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(
                Cipher.DECRYPT_MODE,
                secretKey(),
                GCMParameterSpec(TAG_BITS, raw, 0, IV_BYTES),
            )
            String(cipher.doFinal(raw, IV_BYTES, raw.size - IV_BYTES), Charsets.UTF_8)
        }
    } catch (_: Exception) {
        // GeneralSecurityException, IOException from KeyStore.load, ProviderException on some
        // devices, bad Base64: all mean "cannot read", never a crash.
        null
    }

    private fun secretKey(): SecretKey {
        val store = KeyStore.getInstance(PROVIDER).apply { load(null) }
        (store.getKey(alias, null) as? SecretKey)?.let { return it }
        val spec = KeyGenParameterSpec.Builder(
            alias,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
            .apply { init(spec) }
            .generateKey()
    }

    companion object {
        const val DEFAULT_ALIAS = "v2c_cloud_keys_v1"
        private const val PROVIDER = "AndroidKeyStore"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val IV_BYTES = 12
        private const val TAG_BITS = 128
    }
}

/**
 * Encrypted API keys, one per service id. Only ciphertext tokens reach the store. No method logs
 * or throws a key, and [toString] never prints one.
 */
class CloudKeyVault(private val store: KeyValueStore, private val cipher: SecretCipher) {

    /** Encrypts and stores the key. False when the Keystore failed (nothing is stored). */
    fun put(serviceId: String, apiKey: String): Boolean {
        val token = try {
            cipher.encrypt(apiKey.trim())
        } catch (_: Exception) {
            return false
        }
        store.put(storeKey(serviceId), token)
        return true
    }

    /** The decrypted key, or null when none is stored or it cannot be decrypted. */
    fun get(serviceId: String): String? {
        val token = store.get(storeKey(serviceId)) ?: return null
        val plain = try {
            cipher.decrypt(token)
        } catch (_: Exception) {
            null
        }
        return plain?.takeIf { it.isNotBlank() }
    }

    fun has(serviceId: String): Boolean = store.get(storeKey(serviceId)) != null

    fun remove(serviceId: String) = store.remove(storeKey(serviceId))

    /** Last four characters for display; null when there is no readable key. */
    fun lastFour(serviceId: String): String? = get(serviceId)?.takeLast(LAST_CHARS)

    override fun toString(): String = "CloudKeyVault"

    private fun storeKey(serviceId: String) = "key.$serviceId"

    companion object {
        const val LAST_CHARS = 4
    }
}
