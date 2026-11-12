package app.v2c.ime.asr

import app.v2c.ime.ErrorClass
import app.v2c.ime.asr.local.LocalAsr
import app.v2c.ime.ffi.AsrPick
import app.v2c.ime.ffi.AsrPlan
import app.v2c.ime.ffi.AsrSource
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.V2cFfi
import app.v2c.ime.ffi.V2cFfiException
import app.v2c.ime.net.Clock
import app.v2c.ime.net.LanDetector

/** Sink for log lines. Lines contain only source names, lengths, latencies and error codes. */
fun interface EventLog {
    fun log(line: String)
}

/** The routing decision for one utterance, taken when recording starts. */
data class Prepared(val lan: LanSite, val plan: AsrPlan, val pick: AsrPick)

enum class RouteFailure {
    /** Empty transcript or too little audio. */
    NO_SPEECH,

    /** LAN recognition failed and the local model cannot take over: the user must speak again. */
    LAN_FAILED_SAY_AGAIN,

    /** Anything else (local model error, bad service id). */
    FAILED,
}

sealed interface RecognizeResult {
    data class Text(val text: String, val source: AsrSource, val fellBack: Boolean) : RecognizeResult
    data class Failure(val reason: RouteFailure) : RecognizeResult
}

/**
 * Per-utterance routing and fallback (plan section 8.1). Pure Kotlin: the FFI, the local model
 * and the clock are injected. [prepare] is cheap and may run on the main thread; [recognizePcm]
 * and [finishSystemText] block and belong on Dispatchers.IO.
 *
 * Fallback rules:
 * - LAN fails -> the same PCM goes to the local model when it is downloaded, else the user is
 *   asked to speak again.
 * - System recognition fails -> it recorded the audio itself, so the local model needs the user
 *   to speak again ([systemFailed] arms a one-shot local pick for the next utterance).
 * - A forced pick never falls back.
 *
 * Privacy: transcripts are returned to the caller and never logged.
 */
class RecognitionRouter(
    private val ffi: V2cFfi,
    private val detector: LanDetector,
    private val prefs: SourcePrefs,
    private val local: LocalAsr,
    private val log: EventLog = EventLog { },
    private val clock: Clock = Clock.System,
) {
    @Volatile private var forceLocalOnce = false

    fun lan(): LanSite = detector.current()

    /** Plan the next utterance would get, without consuming the one-shot local pick. */
    fun preview(systemAvailable: Boolean): AsrPlan =
        ffi.chooseAsr(detector.current(), systemAvailable, local.isReady(), effectivePick())

    /** Takes the routing decision for one utterance. */
    fun prepare(systemAvailable: Boolean): Prepared {
        val pick = effectivePick()
        forceLocalOnce = false
        val lan = detector.current()
        val plan = ffi.chooseAsr(lan, systemAvailable, local.isReady(), pick)
        log.log("route lan=$lan pick=$pick primary=${plan.primary} fallback=${plan.fallback} hint=${plan.hint}")
        return Prepared(lan, plan, pick)
    }

    /** Call when the user changes the pick: a pending one-shot local pick no longer applies. */
    fun pickChanged() {
        forceLocalOnce = false
    }

    private fun effectivePick(): AsrPick = if (forceLocalOnce) AsrPick.LOCAL else prefs.pick

    /**
     * The system recognizer failed with [errorClass]. Returns true when the next utterance will
     * go to the local model and the user must speak again.
     */
    fun systemFailed(prepared: Prepared, errorClass: ErrorClass): Boolean {
        val retryable = errorClass == ErrorClass.LANGUAGE ||
            errorClass == ErrorClass.NETWORK ||
            errorClass == ErrorClass.OTHER
        val useLocal = retryable && prepared.plan.fallback == AsrSource.LOCAL && local.isReady()
        if (useLocal) forceLocalOnce = true
        log.log("system failed class=$errorClass local_next=$useLocal")
        return useLocal
    }

    /** Converts the system recognizer's text to zh-TW. */
    fun finishSystemText(text: String): RecognizeResult {
        val started = clock.nowMs()
        return try {
            val out = ffi.toZhTw(text)
            log.log("system done chars=${out.length} ms=${clock.nowMs() - started}")
            if (out.isBlank()) RecognizeResult.Failure(RouteFailure.NO_SPEECH)
            else RecognizeResult.Text(out, AsrSource.SYSTEM, fellBack = false)
        } catch (e: V2cFfiException) {
            // Keep the unconverted text rather than losing the user's sentence.
            log.log("toZhTw failed code=${e.code}")
            if (text.isBlank()) RecognizeResult.Failure(RouteFailure.NO_SPEECH)
            else RecognizeResult.Text(text, AsrSource.SYSTEM, fellBack = false)
        }
    }

    /** Recognizes recorded 16 kHz mono PCM through the plan's primary (LAN or local) source. */
    fun recognizePcm(prepared: Prepared, pcm: ShortArray): RecognizeResult {
        if (pcm.size < MIN_SAMPLES) {
            log.log("pcm too short samples=${pcm.size}")
            return RecognizeResult.Failure(RouteFailure.NO_SPEECH)
        }
        val plan = prepared.plan
        return when (plan.primary) {
            AsrSource.LAN_TEA, AsrSource.LAN_BREEZE -> {
                val lanResult = runLan(prepared, plan.primary, pcm)
                when {
                    lanResult != null -> finish(lanResult, plan.primary, fellBack = false)
                    plan.fallback == AsrSource.LOCAL && local.isReady() -> {
                        val localText = runLocal(pcm)
                        if (localText == null) RecognizeResult.Failure(RouteFailure.FAILED)
                        else finish(localText, AsrSource.LOCAL, fellBack = true)
                    }
                    else -> RecognizeResult.Failure(RouteFailure.LAN_FAILED_SAY_AGAIN)
                }
            }
            AsrSource.LOCAL -> {
                val t = runLocal(pcm)
                if (t == null) RecognizeResult.Failure(RouteFailure.FAILED) else finish(t, AsrSource.LOCAL, false)
            }
            AsrSource.SYSTEM, AsrSource.NONE -> RecognizeResult.Failure(RouteFailure.FAILED)
        }
    }

    private fun finish(text: String, source: AsrSource, fellBack: Boolean): RecognizeResult =
        if (text.isBlank()) RecognizeResult.Failure(RouteFailure.NO_SPEECH)
        else RecognizeResult.Text(text, source, fellBack)

    /** Raw LAN transcript converted to zh-TW, or null on any failure. */
    private fun runLan(prepared: Prepared, source: AsrSource, pcm: ShortArray): String? {
        val started = clock.nowMs()
        return try {
            val id = ffi.lanServiceId(prepared.lan, source)
            if (id == null) {
                log.log("lan no service id source=$source")
                return null
            }
            val pieces = PcmChunker.split(pcm)
            val raw = pieces.joinToString("") {
                ffi.transcribePcm(PcmChunker.toLittleEndianBytes(it), PcmChunker.SAMPLE_RATE, id)
            }
            val text = ffi.toZhTw(raw)
            log.log("lan done source=$source chunks=${pieces.size} pcm=${pcm.size} chars=${text.length} ms=${clock.nowMs() - started}")
            text
        } catch (e: V2cFfiException) {
            log.log("lan failed source=$source code=${e.code} ms=${clock.nowMs() - started}")
            null
        }
    }

    /** Local-model transcript (already zh-TW converted by the local ASR), or null on failure. */
    private fun runLocal(pcm: ShortArray): String? {
        val started = clock.nowMs()
        return try {
            val pieces = PcmChunker.split(pcm)
            val text = pieces.joinToString("") { local.transcribe(it, PcmChunker.SAMPLE_RATE) }
            log.log("local done chunks=${pieces.size} pcm=${pcm.size} chars=${text.length} ms=${clock.nowMs() - started}")
            text
        } catch (e: Exception) {
            // Class name only: the message of a model error could in theory echo input.
            log.log("local failed type=${e.javaClass.simpleName} ms=${clock.nowMs() - started}")
            null
        }
    }

    companion object {
        /** Below 0.3 s there is nothing to recognize. */
        const val MIN_SAMPLES = PcmChunker.SAMPLE_RATE * 3 / 10
    }
}
