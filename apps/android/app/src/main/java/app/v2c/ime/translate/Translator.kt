package app.v2c.ime.translate

import app.v2c.ime.asr.EventLog
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.V2cFfi
import app.v2c.ime.ffi.V2cFfiException
import app.v2c.ime.net.Clock
import app.v2c.ime.net.LanCatalog
import app.v2c.ime.net.LanDetector

/** State of the Chinese-to-English key. */
data class TranslateKeyState(val enabled: Boolean) {
    companion object {
        /** The key is only usable on a company LAN (Qwen is not reachable elsewhere). */
        fun of(lan: LanSite): TranslateKeyState = TranslateKeyState(lan != LanSite.UNREACHABLE)
    }
}

enum class TranslateFailure {
    /** No company LAN: the keyboard says "需要連上公司網路". */
    NEEDS_LAN,
    EMPTY,
    FAILED,
}

sealed interface TranslateResult {
    data class Text(val text: String) : TranslateResult
    data class Failure(val reason: TranslateFailure) : TranslateResult
}

/**
 * Chinese -> English through the current LAN's Qwen (FFI `translate`, plan section 8.2).
 * [translate] blocks; call it on Dispatchers.IO. Never logs text.
 */
class Translator(
    private val ffi: V2cFfi,
    private val detector: LanDetector,
    private val catalog: LanCatalog,
    private val log: EventLog = EventLog { },
    private val clock: Clock = Clock.System,
) {
    fun keyState(): TranslateKeyState = TranslateKeyState.of(detector.current())

    fun translate(chinese: String): TranslateResult {
        if (chinese.isBlank()) return TranslateResult.Failure(TranslateFailure.EMPTY)
        val lan = detector.current()
        val id = catalog.llmServiceId(lan) ?: return TranslateResult.Failure(TranslateFailure.NEEDS_LAN)
        val started = clock.nowMs()
        return try {
            val out = ffi.translate(chinese, id)
            log.log("translate done lan=$lan in=${chinese.length} out=${out.length} ms=${clock.nowMs() - started}")
            if (out.isBlank()) TranslateResult.Failure(TranslateFailure.EMPTY) else TranslateResult.Text(out)
        } catch (e: V2cFfiException) {
            log.log("translate failed lan=$lan code=${e.code} ms=${clock.nowMs() - started}")
            TranslateResult.Failure(TranslateFailure.FAILED)
        }
    }
}
