package app.v2c.ime.asr

import app.v2c.ime.ErrorClass
import app.v2c.ime.cloud.CloudRouting
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

    /** Cloud recognition failed and nothing else can take over: the user must speak again. */
    CLOUD_FAILED_SAY_AGAIN,

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
 * - LAN fails -> the same PCM goes to the cloud service (when ready), then to the local model
 *   (when downloaded); if neither exists the user is asked to speak again.
 * - Cloud (primary, LAN unreachable) fails -> the same PCM goes to the local model when it is
 *   downloaded, else the user is asked to speak again.
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
    private val cloud: CloudRouting = CloudRouting.None,
) {
    @Volatile private var forceLocalOnce = false

    fun lan(): LanSite = detector.current()

    /** Plan the next utterance would get, without consuming the one-shot local pick. */
    fun preview(systemAvailable: Boolean): AsrPlan =
        ffi.chooseAsr(detector.current(), cloud.asrReady(), systemAvailable, local.isReady(), effectivePick())

    /** Takes the routing decision for one utterance. */
    fun prepare(systemAvailable: Boolean): Prepared {
        val pick = effectivePick()
        forceLocalOnce = false
        val lan = detector.current()
        val plan = ffi.chooseAsr(lan, cloud.asrReady(), systemAvailable, local.isReady(), pick)
        log.log(
            "route lan=$lan pick=$pick primary=${plan.primary} fallback=${plan.fallback} " +
                "fallback2=${plan.fallback2} hint=${plan.hint}",
        )
        return Prepared(lan, plan, pick)
    }

    /** Call when the user changes the pick: a pending one-shot local pick no longer applies. */
    fun pickChanged() {
        forceLocalOnce = false
    }

    private fun effectivePick(): AsrPick = when {
        forceLocalOnce -> AsrPick.LOCAL
        // The cloud service was removed after the user forced it: back to automatic.
        prefs.pick == AsrPick.CLOUD && !cloud.asrConfigured -> AsrPick.AUTO
        else -> prefs.pick
    }

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
        if (text.isBlank()) return RecognizeResult.Failure(RouteFailure.NO_SPEECH)
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
                if (lanResult != null) {
                    finish(lanResult, plan.primary, fellBack = false)
                } else {
                    fallBack(plan, pcm, RouteFailure.LAN_FAILED_SAY_AGAIN)
                }
            }
            AsrSource.CLOUD -> {
                val cloudResult = runCloud(pcm)
                if (cloudResult != null) {
                    finish(cloudResult, AsrSource.CLOUD, fellBack = false)
                } else {
                    fallBack(plan, pcm, RouteFailure.CLOUD_FAILED_SAY_AGAIN)
                }
            }
            AsrSource.LOCAL -> {
                val t = runLocal(pcm)
                if (t == null) RecognizeResult.Failure(RouteFailure.FAILED) else finish(t, AsrSource.LOCAL, false)
            }
            AsrSource.SYSTEM, AsrSource.NONE -> RecognizeResult.Failure(RouteFailure.FAILED)
        }
    }

    /**
     * The primary source failed: try the plan's fallbacks in order with the same PCM (plan
     * section 8.1). A failing local model ends the chain as FAILED; with no usable fallback the
     * user has to speak again ([sayAgain]).
     */
    private fun fallBack(plan: AsrPlan, pcm: ShortArray, sayAgain: RouteFailure): RecognizeResult {
        for (source in listOf(plan.fallback, plan.fallback2)) {
            when {
                source == AsrSource.CLOUD && plan.primary != AsrSource.CLOUD && cloud.asrReady() -> {
                    val text = runCloud(pcm)
                    if (text != null) return finish(text, AsrSource.CLOUD, fellBack = true)
                }
                source == AsrSource.LOCAL && local.isReady() -> {
                    val text = runLocal(pcm) ?: return RecognizeResult.Failure(RouteFailure.FAILED)
                    return finish(text, AsrSource.LOCAL, fellBack = true)
                }
            }
        }
        return RecognizeResult.Failure(sayAgain)
    }

    private fun finish(text: String, source: AsrSource, fellBack: Boolean): RecognizeResult =
        if (text.isBlank()) RecognizeResult.Failure(RouteFailure.NO_SPEECH)
        else RecognizeResult.Text(text, source, fellBack)

    /** Raw LAN transcript converted to zh-TW, or null on any failure. */
    private fun runLan(prepared: Prepared, source: AsrSource, pcm: ShortArray): String? {
        val id = ffi.lanServiceId(prepared.lan, source)
        if (id == null) {
            log.log("lan no service id source=$source")
            return null
        }
        return runService("lan source=$source", id, pcm)
    }

    /** Cloud transcript converted to zh-TW, or null on any failure. */
    private fun runCloud(pcm: ShortArray): String? {
        val id = cloud.asrServiceId()
        if (id == null) {
            log.log("cloud no service id")
            return null
        }
        return runService("cloud source=${cloud.asrLabel()}", id, pcm)
    }

    /** Same PCM through one FFI service id; [tag] is a log prefix with names only. */
    private fun runService(tag: String, id: String, pcm: ShortArray): String? {
        val started = clock.nowMs()
        return try {
            val pieces = PcmChunker.split(pcm)
            val raw = pieces.joinToString("") {
                ffi.transcribePcm(PcmChunker.toLittleEndianBytes(it), PcmChunker.SAMPLE_RATE, id)
            }
            // A blank raw transcript is "no speech"; do not convert it into something non-blank.
            val text = if (raw.isBlank()) "" else ffi.toZhTw(raw)
            log.log("$tag done chunks=${pieces.size} pcm=${pcm.size} chars=${text.length} ms=${clock.nowMs() - started}")
            text
        } catch (e: V2cFfiException) {
            log.log("$tag failed code=${e.code} ms=${clock.nowMs() - started}")
            null
        }
    }

    /** Local-model transcript (already zh-TW converted by the local ASR), or null on failure. */
    private fun runLocal(pcm: ShortArray): String? {
        val started = clock.nowMs()
        return try {
            val pieces = PcmChunker.split(pcm)
            val raw = pieces.joinToString("") { local.transcribe(it, PcmChunker.SAMPLE_RATE) }
            val text = if (raw.isBlank()) "" else raw
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
