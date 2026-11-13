package app.v2c.ime.cloud

import androidx.annotation.StringRes
import app.v2c.ime.R
import app.v2c.ime.ffi.CloudUse
import app.v2c.ime.ffi.FfiErrorCode
import app.v2c.ime.ffi.V2cFfi
import app.v2c.ime.ffi.V2cFfiException
import java.net.URI

/** Why a draft cannot be saved or tested yet. */
enum class DraftProblem(@StringRes val message: Int) {
    NO_USE(R.string.cloud_problem_no_use),
    KEY_MISSING(R.string.cloud_problem_key),
    MODEL_MISSING(R.string.cloud_problem_model),
    BASE_URL_INVALID(R.string.cloud_problem_base_url),
}

/** Result of testing one use of a draft service. */
data class UseTestResult(val use: CloudUse, val latencyMs: Long, val error: FfiErrorCode?)

/** Pure rules of the cloud setup page (no Android UI), unit tested. */
object CloudSetupLogic {

    /** Claude cannot take audio, so it can only translate. */
    fun allowedUses(kind: CloudKind): Set<CloudUse> =
        if (kind.supportsAsr) setOf(CloudUse.ASR, CloudUse.TRANSLATE) else setOf(CloudUse.TRANSLATE)

    fun normalizeUses(kind: CloudKind, uses: Set<CloudUse>): Set<CloudUse> = uses.intersect(allowedUses(kind))

    /**
     * First problem that stops the draft from being tested or saved, or null. [keyAvailable] is
     * true when a key was typed or one is already stored.
     */
    fun validate(draft: CloudServiceSettings, keyAvailable: Boolean): DraftProblem? {
        val uses = normalizeUses(draft.kind, draft.uses)
        return when {
            uses.isEmpty() -> DraftProblem.NO_USE
            draft.kind.keyRequired && !keyAvailable -> DraftProblem.KEY_MISSING
            CloudUse.TRANSLATE in uses && draft.model.isBlank() -> DraftProblem.MODEL_MISSING
            CloudUse.ASR in uses && draft.kind == CloudKind.CUSTOM && draft.asrModel.isBlank() ->
                DraftProblem.MODEL_MISSING
            draft.kind == CloudKind.CUSTOM && !validCustomUrl(draft.baseUrl) -> DraftProblem.BASE_URL_INVALID
            else -> null
        }
    }

    /** Mirrors the FFI rule: HTTPS, or plain HTTP on loopback only. */
    fun validCustomUrl(url: String): Boolean {
        val uri = try {
            URI(url.trim())
        } catch (_: java.net.URISyntaxException) {
            return false
        }
        val host = uri.host?.lowercase() ?: return false
        return when (uri.scheme?.lowercase()) {
            "https" -> true
            "http" -> host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"
            else -> false
        }
    }

    /** "••••abcd" for a stored key. */
    fun maskedKey(lastFour: String?): String? = lastFour?.let { "••••$it" }

    /** Short, user-facing reason for a failed test (never contains the key or a server message). */
    @StringRes
    fun testErrorMessage(code: FfiErrorCode): Int = when (code) {
        FfiErrorCode.UNAUTHORIZED -> R.string.cloud_test_unauthorized
        FfiErrorCode.RATE_LIMITED -> R.string.cloud_test_rate_limited
        FfiErrorCode.UNREACHABLE -> R.string.cloud_test_unreachable
        FfiErrorCode.UNSUPPORTED -> R.string.cloud_test_unsupported
        FfiErrorCode.TIMEOUT -> R.string.cloud_test_timeout
        FfiErrorCode.SERVER -> R.string.cloud_test_server
        FfiErrorCode.BAD_RESPONSE -> R.string.cloud_test_bad_response
        FfiErrorCode.CONFIG -> R.string.cloud_test_config
        else -> R.string.cloud_test_other
    }

    /**
     * Pushes [draft] (with [typedKey], else the stored key from [saved]) next to the other saved
     * services into the FFI, then tests each of its uses. Blocking: Dispatchers.IO. The caller
     * must reload the saved set afterwards ([CloudRuntime.reload]) because the draft stays in
     * Rust memory until then.
     */
    fun testDraft(
        ffi: V2cFfi,
        draft: CloudServiceSettings,
        typedKey: String?,
        saved: CloudSnapshot,
    ): List<UseTestResult> {
        val uses = normalizeUses(draft.kind, draft.uses).sorted()
        val clean = draft.copy(uses = uses.toSet())
        val configs = saved.services.filter { it.kind != draft.kind && it.uses.isNotEmpty() }.map { it.toConfig() } +
            clean.toConfig()
        val keys = HashMap(saved.keysForFfi())
        keys.remove(clean.id)
        val key = typedKey?.trim()?.takeIf { it.isNotEmpty() } ?: saved.keysForFfi()[clean.id]
        if (key != null) keys[clean.id] = key
        return try {
            ffi.setCloudServices(configs, keys)
            uses.map {
                val r = ffi.testConnection(clean.id, it)
                UseTestResult(it, r.latencyMs, r.error)
            }
        } catch (e: V2cFfiException) {
            uses.map { UseTestResult(it, 0, e.code) }
        }
    }
}
