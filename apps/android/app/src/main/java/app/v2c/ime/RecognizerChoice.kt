package app.v2c.ime

/** Which system recognizer to use for the next utterance. */
enum class RecognizerKind { ON_DEVICE, SYSTEM, NONE }

/** Platform-independent classification of SpeechRecognizer error codes. */
enum class ErrorClass { LANGUAGE, NO_SPEECH, NETWORK, PERMISSION, OTHER }

data class ErrorOutcome(val kind: ErrorKind, val markOnDeviceUnsupported: Boolean)

/**
 * Pure selection / fallback policy for the system recognizer (no Android dependencies).
 *
 * `isOnDeviceRecognitionAvailable()` only says an on-device service exists, not that zh-TW is
 * installed. If the on-device recognizer reports a language error, [onDeviceUnsupported] is set
 * for the rest of the service lifetime and the regular system recognizer is used instead.
 */
object RecognizerChoice {

    fun choose(
        onDeviceServiceAvailable: Boolean,
        systemRecognitionAvailable: Boolean,
        onDeviceUnsupported: Boolean,
    ): RecognizerKind = when {
        onDeviceServiceAvailable && !onDeviceUnsupported -> RecognizerKind.ON_DEVICE
        systemRecognitionAvailable -> RecognizerKind.SYSTEM
        else -> RecognizerKind.NONE
    }

    /**
     * Maps a recognizer error to what the UI should show, given the recognizer that was active
     * when the error arrived. Language errors never produce the install hint.
     */
    fun resolveError(
        current: RecognizerKind,
        errorClass: ErrorClass,
        systemRecognitionAvailable: Boolean,
    ): ErrorOutcome = when (errorClass) {
        ErrorClass.LANGUAGE ->
            if (shouldFallBackToSystem(current, true, systemRecognitionAvailable)) {
                ErrorOutcome(ErrorKind.SAY_AGAIN, markOnDeviceUnsupported = true)
            } else {
                ErrorOutcome(ErrorKind.LANGUAGE_UNSUPPORTED, markOnDeviceUnsupported = false)
            }
        ErrorClass.NO_SPEECH -> ErrorOutcome(ErrorKind.NO_SPEECH, false)
        ErrorClass.NETWORK -> ErrorOutcome(ErrorKind.NETWORK, false)
        ErrorClass.PERMISSION -> ErrorOutcome(ErrorKind.NO_PERMISSION, false)
        ErrorClass.OTHER -> ErrorOutcome(ErrorKind.GENERIC, false)
    }

    /** True when the failure should switch away from the on-device recognizer and ask to retry. */
    fun shouldFallBackToSystem(
        current: RecognizerKind,
        languageError: Boolean,
        systemRecognitionAvailable: Boolean,
    ): Boolean = current == RecognizerKind.ON_DEVICE && languageError && systemRecognitionAvailable
}
