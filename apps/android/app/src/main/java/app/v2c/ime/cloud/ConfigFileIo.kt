package app.v2c.ime.cloud

import androidx.annotation.StringRes
import app.v2c.ime.R
import app.v2c.ime.ffi.CloudUse
import app.v2c.ime.ffi.ConfigFileExport
import app.v2c.ime.ffi.ConfigFilePreview
import app.v2c.ime.ffi.ImportStatus
import app.v2c.ime.ffi.ImportedService
import app.v2c.ime.ffi.SkipReason
import app.v2c.ime.ffi.V2cFfi
import java.io.IOException
import java.io.InputStream
import java.util.Calendar

/** Outcome of [ConfigFileIo.apply]. Ids only, no keys. */
data class ApplyResult(
    /** Services written (new or changed). */
    val saved: Int,
    /** Services whose settings or key could not be stored (Keystore failure). */
    val failed: List<CloudKind>,
)

/**
 * Import and export of the portable configuration file (plan section 14, `docs/config-file.md`)
 * for the cloud services of the setup page. The rules (what the phone takes, ids, models,
 * route to uses) are decided in Rust ([V2cFfi.parseConfigFile]); this object reads the current
 * settings for it and writes the result through [CloudRepository]. Keys are never exported and
 * never leave this object except into the repository's Keystore vault. Nothing here logs file
 * content or keys.
 */
object ConfigFileIo {
    /** Same limit as the Rust side (`portable::MAX_FILE_BYTES`). */
    const val MAX_BYTES = 256 * 1024

    /** Reads at most [MAX_BYTES] as UTF-8; null when the stream is larger or cannot be read. */
    fun readLimited(input: InputStream): String? {
        val buf = java.io.ByteArrayOutputStream()
        val chunk = ByteArray(8 * 1024)
        while (true) {
            val n = try {
                input.read(chunk)
            } catch (_: IOException) {
                return null
            }
            if (n < 0) break
            if (buf.size() + n > MAX_BYTES) return null
            buf.write(chunk, 0, n)
        }
        return String(buf.toByteArray(), Charsets.UTF_8)
    }

    fun storedKeyIds(repo: CloudRepository): List<String> =
        CloudKind.entries.filter { repo.hasKey(it) }.map { it.serviceId }

    /** Parses [text] against the current settings. Blocking (FFI): Dispatchers.IO. */
    fun preview(ffi: V2cFfi, repo: CloudRepository, text: String): ConfigFilePreview =
        ffi.parseConfigFile(text, repo.services().map { it.toConfig() }, storedKeyIds(repo))

    /**
     * Writes the services of [preview] (merge by provider; other services and keys stay).
     * [typedKeys] maps the phone id (`cloud-gemini` ...) to a key the user pasted; blank is
     * ignored. Rules:
     * - [ImportedService.dropOldKey]: the stored key goes first, so it can never reach a new host.
     * - Uses are the existing ones plus the file's; [CloudRepository.save] takes a use away from
     *   other services, so there is still at most one service per use.
     * - An unchanged service with nothing new to write is left alone.
     */
    fun apply(repo: CloudRepository, preview: ConfigFilePreview, typedKeys: Map<String, String>): ApplyResult {
        var saved = 0
        val failed = mutableListOf<CloudKind>()
        for (item in preview.services) {
            val kind = CloudKind.entries.firstOrNull { it.serviceId == item.config.id } ?: continue
            val existing = repo.service(kind)
            val typed = typedKeys[kind.serviceId]?.trim()?.takeIf { it.isNotEmpty() }
            val uses = CloudSetupLogic.normalizeUses(kind, (existing?.uses ?: emptySet()) + item.config.uses)
            val settings = CloudServiceSettings(
                kind = kind,
                model = item.config.model.ifBlank { kind.suggestedModel },
                asrModel = item.config.asrModel ?: kind.suggestedAsrModel,
                baseUrl = item.config.baseUrl ?: "",
                uses = uses,
            )
            val nothingToWrite = item.status == ImportStatus.UNCHANGED && existing != null &&
                existing.uses == uses && typed == null && !item.dropOldKey
            if (nothingToWrite) continue
            if (item.dropOldKey) repo.removeKey(kind)
            if (repo.save(settings, typed)) saved++ else failed += kind
        }
        return ApplyResult(saved, failed)
    }

    /** The phone's services as file text. Keys are not read at all, only which ids have one. */
    fun export(ffi: V2cFfi, repo: CloudRepository): ConfigFileExport {
        val services = repo.services()
        return ffi.exportConfigFile(
            services = services.map { it.toConfig() },
            asrId = services.firstOrNull { it.uses.contains(CloudUse.ASR) }?.id,
            translateId = services.firstOrNull { it.uses.contains(CloudUse.TRANSLATE) }?.id,
            storedKeys = storedKeyIds(repo),
        )
    }

    /** `v2c-config-YYYYMMDD.toml` */
    fun defaultFileName(now: Calendar = Calendar.getInstance()): String =
        "v2c-config-%04d%02d%02d.toml".format(
            now.get(Calendar.YEAR),
            now.get(Calendar.MONTH) + 1,
            now.get(Calendar.DAY_OF_MONTH),
        )

    /** Services that get a masked key field: no key yet, or the old key is dropped. */
    fun needingKeys(preview: ConfigFilePreview): List<ImportedService> = preview.services.filter { it.needsKey || it.dropOldKey }

    /** True when applying changes something (a service or a use). */
    fun hasChanges(repo: CloudRepository, preview: ConfigFilePreview): Boolean = preview.services.any { s ->
        val existing = repo.services().firstOrNull { it.id == s.config.id }
        s.status != ImportStatus.UNCHANGED || s.needsKey || existing == null || !existing.uses.containsAll(s.config.uses)
    }

    @StringRes
    fun skipMessage(reason: SkipReason): Int = when (reason) {
        SkipReason.BUILTIN -> R.string.cfg_skip_builtin
        SkipReason.HTTP_ENDPOINT -> R.string.cfg_skip_http
        SkipReason.OFFICIAL_BASE_URL -> R.string.cfg_skip_base_url
        SkipReason.DUPLICATE_PROVIDER -> R.string.cfg_skip_duplicate
    }

    /** Label for an ignored part (`route.polish`, `vocabulary`, ...); null shows the raw name. */
    @StringRes
    fun ignoredLabel(name: String): Int? = when (name) {
        "route.polish" -> R.string.cfg_ignored_polish
        "vocabulary" -> R.string.cfg_ignored_vocab
        "route.asr", "route.translate" -> R.string.cfg_ignored_route_skipped
        "preset.default_keys" -> R.string.cfg_ignored_default_keys
        else -> null
    }

    /** User-facing reason for a rejected file (never echoes file content). */
    @StringRes
    fun errorMessage(code: String): Int = when (code) {
        "too_large" -> R.string.cfg_err_too_large
        "syntax" -> R.string.cfg_err_syntax
        "format_missing", "format_invalid" -> R.string.cfg_err_format
        "format_too_new" -> R.string.cfg_err_too_new
        "unknown_kind" -> R.string.cfg_err_kind
        else -> R.string.cfg_err_other
    }
}
