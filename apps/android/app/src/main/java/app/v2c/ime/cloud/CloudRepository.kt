package app.v2c.ime.cloud

import android.content.Context
import app.v2c.ime.ffi.CloudServiceConfig
import app.v2c.ime.ffi.CloudUse

/**
 * What the keyboard and the FFI need from the stored settings: the services plus the readable
 * keys. [toString] never prints a key.
 */
class CloudSnapshot(
    val services: List<CloudServiceSettings>,
    private val keys: Map<String, String>,
) {
    /** Services that can be handed to the FFI (at least one use). */
    fun configs(): List<CloudServiceConfig> = services.filter { it.uses.isNotEmpty() }.map { it.toConfig() }

    /** id to key, for [app.v2c.ime.ffi.V2cFfi.setCloudServices]. Do not log. */
    fun keysForFfi(): Map<String, String> = keys

    fun serviceFor(use: CloudUse): CloudServiceSettings? = services.firstOrNull { use in it.uses }

    /** True when the service has a readable key, or its provider does not need one. */
    fun keyReadable(service: CloudServiceSettings): Boolean =
        !service.kind.keyRequired || keys.containsKey(service.id)

    /** The service for [use] when its key is readable. */
    fun readyServiceFor(use: CloudUse): CloudServiceSettings? =
        serviceFor(use)?.takeIf { keyReadable(it) }

    override fun toString(): String = "CloudSnapshot(services=${services.size})"

    companion object {
        val EMPTY = CloudSnapshot(emptyList(), emptyMap())
    }
}

/**
 * Cloud service settings in the app-private preferences plus the encrypted keys in [vault].
 * Settings never contain keys. At most one service is used for recognition and at most one for
 * translation; saving a service that takes a use removes it from the others.
 */
class CloudRepository(
    private val store: KeyValueStore,
    private val vault: CloudKeyVault,
) {
    fun services(): List<CloudServiceSettings> =
        (store.get(KEY_KINDS) ?: "")
            .split(',')
            .mapNotNull { CloudKind.parse(it) }
            .distinct()
            .map { read(it) }

    fun service(kind: CloudKind): CloudServiceSettings? = services().firstOrNull { it.kind == kind }

    /**
     * Stores [settings] and, when [apiKey] is not blank, its key (blank keeps the existing key).
     * Uses are normalized with [CloudSetupLogic.normalizeUses] and taken away from other services.
     */
    fun save(settings: CloudServiceSettings, apiKey: String?) {
        val clean = settings.copy(uses = CloudSetupLogic.normalizeUses(settings.kind, settings.uses))
        val others = services().filter { it.kind != clean.kind }.map { other ->
            other.copy(uses = other.uses - clean.uses)
        }
        (others + clean).forEach { write(it) }
        store.put(KEY_KINDS, (others + clean).joinToString(",") { it.kind.name })
        if (!apiKey.isNullOrBlank()) vault.put(clean.id, apiKey)
    }

    /** Removes the service and its key. */
    fun remove(kind: CloudKind) {
        val rest = services().filter { it.kind != kind }
        store.put(KEY_KINDS, rest.joinToString(",") { it.kind.name })
        listOf(F_MODEL, F_ASR_MODEL, F_BASE_URL, F_USES).forEach { store.remove("${kind.name}.$it") }
        vault.remove(kind.serviceId)
    }

    fun hasKey(kind: CloudKind): Boolean = vault.has(kind.serviceId)

    fun lastFour(kind: CloudKind): String? = vault.lastFour(kind.serviceId)

    /** Settings plus every key that can be decrypted. */
    fun snapshot(): CloudSnapshot {
        val list = services()
        val keys = HashMap<String, String>()
        for (s in list) vault.get(s.id)?.let { keys[s.id] = it }
        return CloudSnapshot(list, keys)
    }

    private fun read(kind: CloudKind): CloudServiceSettings {
        val uses = (store.get("${kind.name}.$F_USES") ?: "")
            .split(',')
            .mapNotNull { n -> CloudUse.entries.firstOrNull { it.name == n } }
            .toSet()
        return CloudServiceSettings(
            kind = kind,
            model = store.get("${kind.name}.$F_MODEL") ?: kind.suggestedModel,
            asrModel = store.get("${kind.name}.$F_ASR_MODEL") ?: kind.suggestedAsrModel,
            baseUrl = store.get("${kind.name}.$F_BASE_URL") ?: "",
            uses = CloudSetupLogic.normalizeUses(kind, uses),
        )
    }

    private fun write(s: CloudServiceSettings) {
        store.put("${s.kind.name}.$F_MODEL", s.model)
        store.put("${s.kind.name}.$F_ASR_MODEL", s.asrModel)
        store.put("${s.kind.name}.$F_BASE_URL", s.baseUrl)
        store.put("${s.kind.name}.$F_USES", s.uses.joinToString(",") { it.name })
    }

    companion object {
        /** Production wiring: app-private preferences plus the Keystore cipher. Android glue. */
        fun create(context: Context): CloudRepository {
            val app = context.applicationContext
            return CloudRepository(
                SharedPrefsKeyValueStore(app, SETTINGS_FILE),
                CloudKeyVault(SharedPrefsKeyValueStore(app, KEYS_FILE), KeystoreSecretCipher()),
            )
        }

        const val SETTINGS_FILE = "v2c_cloud"
        const val KEYS_FILE = "v2c_cloud_keys"
        private const val KEY_KINDS = "kinds"
        private const val F_MODEL = "model"
        private const val F_ASR_MODEL = "asr_model"
        private const val F_BASE_URL = "base_url"
        private const val F_USES = "uses"
    }
}
