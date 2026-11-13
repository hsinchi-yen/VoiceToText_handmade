package app.v2c.ime.ffi

/**
 * Plain-Kotlin facade over the uniffi bindings (package `uniffi.v2c_ffi`, crates/v2c-ffi).
 *
 * Everything outside [UniffiV2cFfi] depends on this interface and these mirror types only, so the
 * decision logic is unit tested on a plain JVM with a fake. All calls are synchronous; callers
 * use Dispatchers.IO (or a worker thread) except the pure [chooseAsr] / [lanServiceId].
 *
 * Privacy: no value here is ever logged; errors carry only a [FfiErrorCode].
 */

/** Result of the LAN probe (mirror of `uniffi.v2c_ffi.Lan`). */
enum class LanSite(val servicePrefix: String) {
    TN("tn-"),
    RD("rd-"),
    UNREACHABLE(""),
}

/** The user's manual choice (mirror of `AsrPick`). */
enum class AsrPick { AUTO, TEA, BREEZE, CLOUD, SYSTEM, LOCAL }

/** A recognition source (mirror of `AsrSource`). */
enum class AsrSource { NONE, LAN_TEA, LAN_BREEZE, CLOUD, SYSTEM, LOCAL }

/** Why no source is offered (mirror of `AsrHint`). */
enum class AsrHint {
    NONE, NO_SOURCE_AVAILABLE, LAN_UNREACHABLE, SYSTEM_UNAVAILABLE, LOCAL_MODEL_NOT_READY, CLOUD_NOT_READY,
}

/** Mirror of `AsrPlan`. */
data class AsrPlan(
    val primary: AsrSource,
    val fallback: AsrSource,
    val retrySpeak: Boolean,
    val hint: AsrHint,
    /** Second fallback (LAN -> [fallback] = CLOUD -> [fallback2] = LOCAL); NONE otherwise. */
    val fallback2: AsrSource = AsrSource.NONE,
)

/** Cloud provider (mirror of `CloudProvider`). */
enum class CloudProvider { GEMINI, OPENAI, CLAUDE, CUSTOM }

/** What a cloud service is used for (mirror of `CloudUse`). */
enum class CloudUse { ASR, TRANSLATE }

/** One cloud service (mirror of `CloudServiceConfig`). The API key travels separately. */
data class CloudServiceConfig(
    val id: String,
    val provider: CloudProvider,
    val model: String,
    val asrModel: String?,
    val baseUrl: String?,
    val uses: List<CloudUse>,
    val asrTimeoutMs: Long? = null,
)

/** Mirror of `TestResult`: [error] is null on success. */
data class CloudTestResult(val latencyMs: Long, val error: FfiErrorCode?)

/** One LAN service from the preset files (mirror of `LanService`; no keys). */
data class LanServiceInfo(
    val id: String,
    val baseUrl: String,
    val hasAsr: Boolean,
    val hasLlm: Boolean,
    val asrTimeoutMs: Long,
)

/** Codes only (mirror of `FfiException` subclasses). */
enum class FfiErrorCode {
    CONFIG, NOT_LOADED, UNKNOWN_SERVICE, UNSUPPORTED, UNAUTHORIZED, RATE_LIMITED,
    UNREACHABLE, BAD_RESPONSE, SERVER, TIMEOUT, INVALID_AUDIO, EMPTY_TEXT, INTERNAL,
}

class V2cFfiException(val code: FfiErrorCode) : Exception(code.name)

interface V2cFfi {
    /** Parses the two preset files; must succeed before [transcribePcm] / [translate]. */
    fun loadLanServices(tnToml: String, rdToml: String): List<LanServiceInfo>

    /** Pure routing decision for one utterance (plan section 8.1). */
    fun chooseAsr(
        lan: LanSite,
        cloudReady: Boolean,
        systemAvailable: Boolean,
        localModelReady: Boolean,
        pick: AsrPick,
    ): AsrPlan

    /**
     * Replaces the cloud service set (never the LAN services). [keys] maps service id to API key
     * and lives in Rust memory only. Throws [V2cFfiException] (CONFIG) on an invalid set.
     */
    fun setCloudServices(services: List<CloudServiceConfig>, keys: Map<String, String>)

    /** Test call for a configured cloud service; errors are returned, not thrown. */
    fun testConnection(serviceId: String, use: CloudUse): CloudTestResult

    /** Service id for a LAN source, or null when [source] is not a LAN source or [lan] is unreachable. */
    fun lanServiceId(lan: LanSite, source: AsrSource): String?

    /** 16 kHz mono PCM16 little-endian bytes -> raw transcript. */
    fun transcribePcm(pcmLe: ByteArray, rate: Int, serviceId: String): String

    fun translate(text: String, serviceId: String): String

    fun toZhTw(text: String): String
}
