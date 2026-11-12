package app.v2c.ime

/** Which system recognizer to use for the next utterance. */
enum class RecognizerKind { ON_DEVICE, SYSTEM, NONE }

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

    /** True when the failure should switch away from the on-device recognizer and ask to retry. */
    fun shouldFallBackToSystem(
        current: RecognizerKind,
        languageError: Boolean,
        systemRecognitionAvailable: Boolean,
    ): Boolean = current == RecognizerKind.ON_DEVICE && languageError && systemRecognitionAvailable
}
