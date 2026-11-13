package app.v2c.ime.translate

import app.v2c.ime.asr.EventLog
import app.v2c.ime.cloud.CloudRouting
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.V2cFfi
import app.v2c.ime.ffi.V2cFfiException
import app.v2c.ime.net.Clock
import app.v2c.ime.net.LanCatalog
import app.v2c.ime.net.LanDetector

/** State of the Chinese-to-English key. */
data class TranslateKeyState(val enabled: Boolean) {
    companion object {
        /**
         * The key is usable on a company LAN (Qwen) or, anywhere else, when a cloud translation
         * service is ready (plan section 8.2).
         */
        fun of(lan: LanSite, cloudTranslateReady: Boolean = false): TranslateKeyState =
            TranslateKeyState(lan != LanSite.UNREACHABLE || cloudTranslateReady)
    }
}

enum class TranslateFailure {
    /** No company LAN and no ready cloud service: the keyboard says what is needed. */
    NEEDS_LAN,
    EMPTY,
    FAILED,
}

sealed interface TranslateResult {
    data class Text(val text: String) : TranslateResult
    data class Failure(val reason: TranslateFailure) : TranslateResult
}

/**
 * Chinese -> English through the current LAN's Qwen, or, when no LAN is reachable, through the
 * configured cloud translation service (FFI `translate`, plan section 8.2). [translate] blocks;
 * call it on Dispatchers.IO. Never logs text.
 */
class Translator(
    private val ffi: V2cFfi,
    private val detector: LanDetector,
    private val catalog: LanCatalog,
    private val log: EventLog = EventLog { },
    private val clock: Clock = Clock.System,
    private val cloud: CloudRouting = CloudRouting.None,
) {
    fun keyState(): TranslateKeyState = TranslateKeyState.of(detector.current(), cloud.translateReady())

    fun translate(chinese: String): TranslateResult {
        if (chinese.isBlank()) return TranslateResult.Failure(TranslateFailure.EMPTY)
        val lan = detector.current()
        // LAN Qwen first; only when no LAN is reachable the cloud service is used.
        val lanId = catalog.llmServiceId(lan)
        val id = lanId
            ?: cloud.takeIf { it.translateReady() }?.translateServiceId()
            ?: return TranslateResult.Failure(TranslateFailure.NEEDS_LAN)
        val via = if (lanId != null) "lan=$lan" else "cloud"
        val started = clock.nowMs()
        return try {
            val out = ffi.translate(chinese, id)
            log.log("translate done $via in=${chinese.length} out=${out.length} ms=${clock.nowMs() - started}")
            if (out.isBlank()) TranslateResult.Failure(TranslateFailure.EMPTY) else TranslateResult.Text(out)
        } catch (e: V2cFfiException) {
            log.log("translate failed $via code=${e.code} ms=${clock.nowMs() - started}")
            TranslateResult.Failure(TranslateFailure.FAILED)
        }
    }
}
