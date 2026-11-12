package app.v2c.ime

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import androidx.annotation.StringRes
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.content.ContextCompat

enum class ErrorKind(@StringRes val message: Int) {
    NO_PERMISSION(R.string.error_no_permission),
    UNAVAILABLE(R.string.error_unavailable),
    NO_SPEECH(R.string.error_no_speech),
    NETWORK(R.string.error_network),
    SAY_AGAIN(R.string.error_say_again),
    GENERIC(R.string.error_generic),
}

sealed interface VoiceUiState {
    data object Idle : VoiceUiState
    data class Recording(val startedAtMs: Long) : VoiceUiState
    data object Recognizing : VoiceUiState
    data class Failed(val kind: ErrorKind) : VoiceUiState
}

/**
 * One keyboard-side voice session backed by the system [SpeechRecognizer] (plan section 8.1,
 * system recognition). Must be used from the main thread.
 *
 * Privacy: the recognized text is only handed to [onText]; it is never logged.
 */
class VoiceSession(
    private val context: Context,
    private val onText: (String) -> Unit,
) {
    var state: VoiceUiState by mutableStateOf(VoiceUiState.Idle)
        private set

    /** Recent normalized mic levels (0..1) for the waveform, newest last. */
    var levels: FloatArray by mutableStateOf(FloatArray(WAVE_BARS))
        private set

    private val detector = TapHoldDetector()
    private var recognizer: SpeechRecognizer? = null
    private var currentKind = RecognizerKind.NONE

    /** Set once the on-device recognizer reports zh-TW unsupported; lasts for the service lifetime. */
    private var onDeviceUnsupported = false

    fun onMicPress() = apply(detector.onPress(SystemClock.uptimeMillis()))

    fun onMicRelease() = apply(detector.onRelease(SystemClock.uptimeMillis()))

    fun dismissError() {
        if (state is VoiceUiState.Failed) state = VoiceUiState.Idle
    }

    fun destroy() {
        releaseRecognizer()
        detector.reset()
        state = VoiceUiState.Idle
    }

    private fun apply(action: TapHoldDetector.Action) {
        when (action) {
            TapHoldDetector.Action.NONE -> Unit
            TapHoldDetector.Action.START -> start()
            TapHoldDetector.Action.STOP -> stop()
        }
    }

    private fun start() {
        if (state is VoiceUiState.Recognizing || state is VoiceUiState.Recording) {
            // A new press while busy is ignored; keep the detector in sync.
            detector.reset()
            return
        }
        if (ContextCompat.checkSelfPermission(context, android.Manifest.permission.RECORD_AUDIO) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            fail(ErrorKind.NO_PERMISSION)
            return
        }
        val rec = createRecognizer()
        if (rec == null) {
            fail(ErrorKind.UNAVAILABLE)
            return
        }
        releaseRecognizer()
        recognizer = rec
        rec.setRecognitionListener(listener)
        levels = FloatArray(WAVE_BARS)
        state = VoiceUiState.Recording(SystemClock.uptimeMillis())
        rec.startListening(buildIntent())
    }

    private fun stop() {
        if (state is VoiceUiState.Recording) {
            state = VoiceUiState.Recognizing
            recognizer?.stopListening()
        }
    }

    private fun systemAvailable() = SpeechRecognizer.isRecognitionAvailable(context)

    private fun createRecognizer(): SpeechRecognizer? {
        val onDevice = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S &&
            SpeechRecognizer.isOnDeviceRecognitionAvailable(context)
        currentKind = RecognizerChoice.choose(onDevice, systemAvailable(), onDeviceUnsupported)
        return when (currentKind) {
            RecognizerKind.ON_DEVICE ->
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                    SpeechRecognizer.createOnDeviceSpeechRecognizer(context)
                } else {
                    null
                }
            RecognizerKind.SYSTEM -> SpeechRecognizer.createSpeechRecognizer(context)
            RecognizerKind.NONE -> null
        }
    }

    private fun releaseRecognizer() {
        recognizer?.destroy()
        recognizer = null
        currentKind = RecognizerKind.NONE
    }

    private val isActive: Boolean
        get() = state is VoiceUiState.Recording || state is VoiceUiState.Recognizing

    private fun buildIntent(): Intent =
        Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            putExtra(RecognizerIntent.EXTRA_LANGUAGE, LANGUAGE_TAG)
            putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
            // No partial results: the keyboard never shows in-progress text.
            putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, false)
            putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1)
            // Tap mode: a short pause must not end the recording. Not every service honours these.
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS, SILENCE_MS)
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_POSSIBLY_COMPLETE_SILENCE_LENGTH_MILLIS, SILENCE_MS)
        }

    private fun fail(kind: ErrorKind) {
        detector.reset()
        state = VoiceUiState.Failed(kind)
    }

    private val listener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) = Unit
        override fun onBeginningOfSpeech() = Unit
        override fun onBufferReceived(buffer: ByteArray?) = Unit
        override fun onPartialResults(partialResults: Bundle?) = Unit
        override fun onEvent(eventType: Int, params: Bundle?) = Unit

        override fun onRmsChanged(rmsdB: Float) {
            // rmsdB is roughly -2..10; keep a rolling window of normalized levels.
            val level = ((rmsdB + 2f) / 12f).coerceIn(0f, 1f)
            val next = FloatArray(WAVE_BARS)
            System.arraycopy(levels, 1, next, 0, WAVE_BARS - 1)
            next[WAVE_BARS - 1] = level
            levels = next
        }

        override fun onEndOfSpeech() {
            // The recognizer decided the utterance is over (silence); behave like a stop.
            if (state is VoiceUiState.Recording) {
                detector.reset()
                state = VoiceUiState.Recognizing
            }
        }

        override fun onResults(results: Bundle?) {
            if (!isActive) return
            val text = results
                ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
                ?.firstOrNull()
            detector.reset()
            releaseRecognizer()
            if (text.isNullOrEmpty()) {
                state = VoiceUiState.Failed(ErrorKind.NO_SPEECH)
            } else {
                onText(text)
                state = VoiceUiState.Idle
            }
        }

        override fun onError(error: Int) {
            // Some services send ERROR_CLIENT / BUSY after results or after destroy; ignore those.
            if (!isActive) return
            val languageError = error == SpeechRecognizer.ERROR_LANGUAGE_NOT_SUPPORTED ||
                error == SpeechRecognizer.ERROR_LANGUAGE_UNAVAILABLE
            if (RecognizerChoice.shouldFallBackToSystem(currentKind, languageError, systemAvailable())) {
                onDeviceUnsupported = true
                releaseRecognizer()
                fail(ErrorKind.SAY_AGAIN)
                return
            }
            releaseRecognizer()
            val kind = when (error) {
                SpeechRecognizer.ERROR_NO_MATCH,
                SpeechRecognizer.ERROR_SPEECH_TIMEOUT -> ErrorKind.NO_SPEECH
                SpeechRecognizer.ERROR_NETWORK,
                SpeechRecognizer.ERROR_NETWORK_TIMEOUT,
                SpeechRecognizer.ERROR_SERVER -> ErrorKind.NETWORK
                SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS -> ErrorKind.NO_PERMISSION
                SpeechRecognizer.ERROR_LANGUAGE_NOT_SUPPORTED,
                SpeechRecognizer.ERROR_LANGUAGE_UNAVAILABLE -> ErrorKind.UNAVAILABLE
                else -> ErrorKind.GENERIC
            }
            fail(kind)
        }
    }

    companion object {
        const val WAVE_BARS = 32
        const val SILENCE_MS = 3_000L
        const val LANGUAGE_TAG = "zh-TW"
    }
}
