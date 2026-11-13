package app.v2c.ime.asr

import app.v2c.ime.ErrorKind
import app.v2c.ime.ffi.AsrHint
import app.v2c.ime.translate.TranslateFailure
import app.v2c.ime.translate.TranslateResult
import app.v2c.ime.translate.Translator

/** What one key does with the recognized text. */
enum class VoiceMode { DICTATE, TRANSLATE }

/** Final outcome of one utterance, ready for the keyboard. */
sealed interface Finished {
    data class Commit(val text: String) : Finished
    data class Error(val kind: ErrorKind) : Finished
}

/** Pure mappings from routing results to what the keyboard shows. */
object ErrorMapping {

    /** Why the plan has no usable source. Null when [hint] is NONE. */
    fun forHint(hint: AsrHint): ErrorKind? = when (hint) {
        AsrHint.NONE -> null
        AsrHint.NO_SOURCE_AVAILABLE -> ErrorKind.NO_SOURCE
        AsrHint.LAN_UNREACHABLE -> ErrorKind.LAN_UNREACHABLE
        AsrHint.SYSTEM_UNAVAILABLE -> ErrorKind.UNAVAILABLE
        AsrHint.LOCAL_MODEL_NOT_READY -> ErrorKind.LOCAL_NOT_READY
        AsrHint.CLOUD_NOT_READY -> ErrorKind.CLOUD_NOT_READY
    }

    fun forFailure(reason: RouteFailure): ErrorKind = when (reason) {
        RouteFailure.NO_SPEECH -> ErrorKind.NO_SPEECH
        RouteFailure.LAN_FAILED_SAY_AGAIN -> ErrorKind.LAN_FAILED
        RouteFailure.CLOUD_FAILED_SAY_AGAIN -> ErrorKind.CLOUD_FAILED
        RouteFailure.FAILED -> ErrorKind.GENERIC
    }

    /**
     * Turns a recognition result into text to commit. In [VoiceMode.TRANSLATE] the Chinese text is
     * sent to the LAN Qwen first and only the English result is committed; a failed translation
     * never commits the Chinese. Blocking; call on Dispatchers.IO.
     */
    fun finish(result: RecognizeResult, mode: VoiceMode, translator: Translator): Finished = when (result) {
        is RecognizeResult.Failure -> Finished.Error(forFailure(result.reason))
        is RecognizeResult.Text -> when (mode) {
            VoiceMode.DICTATE -> Finished.Commit(result.text)
            VoiceMode.TRANSLATE -> when (val t = translator.translate(result.text)) {
                is TranslateResult.Text -> Finished.Commit(t.text)
                is TranslateResult.Failure -> Finished.Error(
                    when (t.reason) {
                        TranslateFailure.NEEDS_LAN -> ErrorKind.NEEDS_LAN
                        TranslateFailure.EMPTY -> ErrorKind.NO_SPEECH
                        TranslateFailure.FAILED -> ErrorKind.TRANSLATE_FAILED
                    },
                )
            }
        }
    }
}
