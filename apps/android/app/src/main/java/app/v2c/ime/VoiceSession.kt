package app.v2c.ime

import android.annotation.SuppressLint
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
import app.v2c.ime.asr.ErrorMapping
import app.v2c.ime.asr.Finished
import app.v2c.ime.asr.Prepared
import app.v2c.ime.asr.RecognitionRouter
import app.v2c.ime.asr.VoiceMode
import app.v2c.ime.cloud.CloudRouting
import app.v2c.ime.ffi.AsrSource
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.recorder.PcmRecorder
import app.v2c.ime.translate.Translator
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

enum class ErrorKind(@StringRes val message: Int) {
    NO_PERMISSION(R.string.error_no_permission),
    UNAVAILABLE(R.string.error_unavailable),
    NO_SPEECH(R.string.error_no_speech),
    NETWORK(R.string.error_network),
    SAY_AGAIN(R.string.error_say_again),
    LANGUAGE_UNSUPPORTED(R.string.error_language_unsupported),
    GENERIC(R.string.error_generic),
    NO_SOURCE(R.string.error_no_source),
    LAN_UNREACHABLE(R.string.error_lan_unreachable),
    LOCAL_NOT_READY(R.string.error_local_not_ready),
    LAN_FAILED(R.string.error_lan_failed),
    CLOUD_NOT_READY(R.string.error_cloud_not_ready),
    CLOUD_FAILED(R.string.error_cloud_failed),
    NEEDS_LAN(R.string.error_needs_lan),
    TRANSLATE_FAILED(R.string.error_translate_failed),
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
    private val scope: CoroutineScope,
    private val router: RecognitionRouter,
    private val recorder: PcmRecorder,
    private val translator: Translator,
    private val cloud: CloudRouting = CloudRouting.None,
    private val onPlanChanged: () -> Unit = {},
    private val onText: (String) -> Unit,
) {
    var state: VoiceUiState by mutableStateOf(VoiceUiState.Idle)
        private set

    /** Recent normalized mic levels (0..1) for the waveform, newest last. */
    var levels: FloatArray by mutableStateOf(FloatArray(WAVE_BARS))
        private set

    /** Current company LAN (cached detection); drives the translate key's enabled state. */
    var lan: LanSite by mutableStateOf(LanSite.UNREACHABLE)
        private set

    /** Bumped when cloud readiness may have changed, so the translate key recomposes. */
    private var cloudVersion by mutableStateOf(0)

    /** The key works on a company LAN (Qwen) or with a ready cloud translation service. */
    val translateEnabled: Boolean
        get() {
            cloudVersion // read for recomposition
            return lan != LanSite.UNREACHABLE || cloud.translateReady()
        }

    fun onLanChanged(site: LanSite) {
        lan = site
    }

    fun onCloudChanged() {
        cloudVersion++
    }

    private val detector = TapHoldDetector()
    private var prepared: Prepared? = null
    private var activeMode = VoiceMode.DICTATE
    private var job: Job? = null
    private var recognizer: SpeechRecognizer? = null
    private var currentKind = RecognizerKind.NONE

    /** Set once the on-device recognizer reports zh-TW unsupported; lasts for the service lifetime. */
    private var onDeviceUnsupported = false

    fun onMicPress() = press(VoiceMode.DICTATE)

    fun onMicRelease() = apply(detector.onRelease(SystemClock.uptimeMillis()))

    /** The Chinese-to-English key behaves like the mic key but commits English. */
    fun onTranslatePress() = press(VoiceMode.TRANSLATE)

    fun onTranslateRelease() = onMicRelease()

    private fun press(mode: VoiceMode) {
        val idle = state !is VoiceUiState.Recording && state !is VoiceUiState.Recognizing
        if (idle && mode == VoiceMode.TRANSLATE && !translateEnabled) {
            fail(ErrorKind.NEEDS_LAN)
            return
        }
        val action = detector.onPress(SystemClock.uptimeMillis())
        if (action == TapHoldDetector.Action.START) activeMode = mode
        apply(action)
    }

    fun dismissError() {
        if (state is VoiceUiState.Failed) state = VoiceUiState.Idle
    }

    fun destroy() {
        job?.cancel()
        job = null
        recorder.cancel()
        prepared = null
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
        // Route this utterance (plan section 8.1): LAN / local record PCM here, system records itself.
        val p = router.prepare(systemAvailable())
        prepared = p
        if (p.plan.primary == AsrSource.NONE) {
            fail(ErrorMapping.forHint(p.plan.hint) ?: ErrorKind.GENERIC)
            return
        }
        if (p.plan.primary != AsrSource.SYSTEM) {
            startPcm()
            return
        }
        // Ordering constraint: release the old recognizer FIRST. releaseRecognizer() resets
        // currentKind, and createRecognizer() sets it; reversing this would leave currentKind
        // as NONE and break the language-error fallback in onError.
        releaseRecognizer()
        val rec = createRecognizer()
        if (rec == null) {
            fail(ErrorKind.UNAVAILABLE)
            return
        }
        recognizer = rec
        rec.setRecognitionListener(listener)
        levels = FloatArray(WAVE_BARS)
        state = VoiceUiState.Recording(SystemClock.uptimeMillis())
        rec.startListening(buildIntent())
    }

    private fun stop() {
        if (state is VoiceUiState.Recording) {
            state = VoiceUiState.Recognizing
            if (prepared?.plan?.primary == AsrSource.SYSTEM) {
                recognizer?.stopListening()
            } else {
                stopPcm()
            }
        }
    }

    private fun startPcm() {
        levels = FloatArray(WAVE_BARS)
        val started = recorder.start(::pushLevel)
        if (!started) {
            fail(ErrorKind.GENERIC)
            return
        }
        state = VoiceUiState.Recording(SystemClock.uptimeMillis())
    }

    private fun stopPcm() {
        val pcm = recorder.stop()
        val p = prepared
        if (p == null) {
            fail(ErrorKind.GENERIC)
            return
        }
        val mode = activeMode
        job?.cancel()
        job = scope.launch {
            val finished = withContext(Dispatchers.IO) {
                ErrorMapping.finish(router.recognizePcm(p, pcm), mode, translator)
            }
            deliver(finished)
        }
    }

    /** Main thread. Dropped when the session was destroyed meanwhile. */
    private fun deliver(finished: Finished) {
        if (state !is VoiceUiState.Recognizing) return
        when (finished) {
            is Finished.Commit -> {
                onText(finished.text)
                state = VoiceUiState.Idle
            }
            is Finished.Error -> fail(finished.kind)
        }
    }

    private fun pushLevel(level: Float) {
        val next = FloatArray(WAVE_BARS)
        System.arraycopy(levels, 1, next, 0, WAVE_BARS - 1)
        next[WAVE_BARS - 1] = level
        levels = next
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
            pushLevel(((rmsdB + 2f) / 12f).coerceIn(0f, 1f))
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
                // zh-TW conversion (and translation for the translate key) runs off the main thread.
                state = VoiceUiState.Recognizing
                val mode = activeMode
                job?.cancel()
                job = scope.launch {
                    val finished = withContext(Dispatchers.IO) {
                        ErrorMapping.finish(router.finishSystemText(text), mode, translator)
                    }
                    deliver(finished)
                }
            }
        }

        override fun onError(error: Int) {
            // Some services send ERROR_CLIENT / BUSY after results or after destroy; ignore those.
            if (!isActive) return
            // Resolve with the still-active kind BEFORE releasing (release resets currentKind).
            val errorClass = classify(error)
            val outcome = RecognizerChoice.resolveError(currentKind, errorClass, systemAvailable())
            if (outcome.markOnDeviceUnsupported) onDeviceUnsupported = true
            releaseRecognizer()
            // The system recognizer recorded the audio itself, so a local-model fallback needs the
            // user to speak again. The on-device -> system switch above takes precedence.
            val toLocal = !outcome.markOnDeviceUnsupported &&
                prepared?.let { router.systemFailed(it, errorClass) } == true
            fail(if (toLocal) ErrorKind.SAY_AGAIN else outcome.kind)
            if (toLocal) onPlanChanged() // chip shows 本機 for the retry
        }

        // ERROR_LANGUAGE_* are API 31 constants; they are inlined ints and older systems never
        // return them, so comparing here is safe.
        @SuppressLint("InlinedApi")
        private fun classify(error: Int): ErrorClass = when (error) {
            SpeechRecognizer.ERROR_LANGUAGE_NOT_SUPPORTED,
            SpeechRecognizer.ERROR_LANGUAGE_UNAVAILABLE -> ErrorClass.LANGUAGE
            SpeechRecognizer.ERROR_NO_MATCH,
            SpeechRecognizer.ERROR_SPEECH_TIMEOUT -> ErrorClass.NO_SPEECH
            SpeechRecognizer.ERROR_NETWORK,
            SpeechRecognizer.ERROR_NETWORK_TIMEOUT,
            SpeechRecognizer.ERROR_SERVER -> ErrorClass.NETWORK
            SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS -> ErrorClass.PERMISSION
            else -> ErrorClass.OTHER
        }
    }

    companion object {
        const val WAVE_BARS = 32
        const val SILENCE_MS = 3_000L
        const val LANGUAGE_TAG = "zh-TW"
    }
}
