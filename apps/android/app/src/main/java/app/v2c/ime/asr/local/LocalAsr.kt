package app.v2c.ime.asr.local

import android.util.Log
import com.k2fsa.sherpa.onnx.FeatureConfig
import com.k2fsa.sherpa.onnx.OfflineModelConfig
import com.k2fsa.sherpa.onnx.OfflineParaformerModelConfig
import com.k2fsa.sherpa.onnx.OfflineRecognizer
import com.k2fsa.sherpa.onnx.OfflineRecognizerConfig
import com.k2fsa.sherpa.onnx.OfflineSenseVoiceModelConfig
import com.k2fsa.sherpa.onnx.OfflineWhisperModelConfig

/** On-device recognition of one utterance. Callers run it off the main thread. */
interface LocalAsr {
    /** True when the model files are present and verified. */
    fun isReady(): Boolean

    /**
     * Recognizes mono 16-bit PCM at [rate] Hz and returns Traditional Chinese (Taiwan) text.
     * Throws [IllegalStateException] when the model is not ready.
     */
    fun transcribe(pcm: ShortArray, rate: Int): String
}

/**
 * Converts recognizer output to zh-TW. In production this is the FFI `to_zh_tw` (T2.3,
 * crates/v2c-ffi/ANDROID.md); tests use a fake. [Identity] is the placeholder until the FFI is
 * wired into Gradle.
 */
fun interface ZhTwConverter {
    fun toZhTw(text: String): String

    object Identity : ZhTwConverter {
        override fun toZhTw(text: String): String = text
    }
}

/** Pure helpers so the PCM and text handling can be unit tested on the JVM. */
object LocalAsrText {
    private val tag = Regex("<\\|[^|]*\\|>")

    /** Removes sherpa-onnx style `<|...|>` language, emotion and event tags and trims. */
    fun clean(raw: String): String = tag.replace(raw, "").trim()

    /** Scales 16-bit samples to the [-1, 1) floats sherpa-onnx expects. */
    fun pcmToFloat(pcm: ShortArray): FloatArray = FloatArray(pcm.size) { pcm[it] / 32768f }
}

/**
 * sherpa-onnx backed [LocalAsr] (Kotlin API, no Rust). The recognizer is created lazily on the
 * first [transcribe] and kept; [release] frees the native memory (call when the IME goes away).
 *
 * Privacy: logs only lengths and timings, never the recognized text.
 */
class SherpaLocalAsr(
    private val downloader: LocalModelDownloader,
    private val zhTw: ZhTwConverter = ZhTwConverter.Identity,
    private val numThreads: Int = DEFAULT_THREADS,
) : LocalAsr {
    private var recognizer: OfflineRecognizer? = null

    override fun isReady(): Boolean = downloader.isReady()

    @Synchronized
    override fun transcribe(pcm: ShortArray, rate: Int): String {
        check(isReady()) { "local model is not ready" }
        if (pcm.isEmpty()) return ""
        val rec = recognizer ?: createRecognizer().also { recognizer = it }
        val started = System.nanoTime()
        val stream = rec.createStream()
        val raw = try {
            stream.acceptWaveform(LocalAsrText.pcmToFloat(pcm), rate)
            rec.decode(stream)
            rec.getResult(stream).text
        } finally {
            stream.release()
        }
        val text = zhTw.toZhTw(LocalAsrText.clean(raw))
        val ms = (System.nanoTime() - started) / 1_000_000
        Log.i(TAG, "local asr model=${downloader.spec.id} pcm=${pcm.size} chars=${text.length} ms=$ms")
        return text
    }

    @Synchronized
    fun release() {
        recognizer?.release()
        recognizer = null
    }

    private fun createRecognizer(): OfflineRecognizer {
        val started = System.nanoTime()
        val rec = OfflineRecognizer(config = buildConfig(downloader, numThreads))
        Log.i(TAG, "local asr loaded model=${downloader.spec.id} ms=${(System.nanoTime() - started) / 1_000_000}")
        return rec
    }

    companion object {
        private const val TAG = "v2c.localasr"
        const val DEFAULT_THREADS = 2

        /** Builds the sherpa-onnx config for a downloaded model; parameters mirror the desktop SenseVoice setup. */
        fun buildConfig(downloader: LocalModelDownloader, numThreads: Int): OfflineRecognizerConfig {
            val spec = downloader.spec
            fun p(name: String) = downloader.path(name).absolutePath
            val model = when (spec.family) {
                ModelFamily.SENSE_VOICE -> OfflineModelConfig(
                    senseVoice = OfflineSenseVoiceModelConfig(
                        model = p("model.int8.onnx"),
                        language = "",
                        useInverseTextNormalization = true,
                    ),
                    tokens = p("tokens.txt"),
                )
                ModelFamily.PARAFORMER -> OfflineModelConfig(
                    paraformer = OfflineParaformerModelConfig(model = p("model.int8.onnx")),
                    tokens = p("tokens.txt"),
                    modelType = "paraformer",
                )
                ModelFamily.WHISPER -> OfflineModelConfig(
                    whisper = OfflineWhisperModelConfig(
                        encoder = p("small-encoder.int8.onnx"),
                        decoder = p("small-decoder.int8.onnx"),
                        language = spec.language,
                    ),
                    tokens = p("small-tokens.txt"),
                    modelType = "whisper",
                )
            }
            model.numThreads = numThreads
            return OfflineRecognizerConfig(featConfig = FeatureConfig(sampleRate = 16000, featureDim = 80), modelConfig = model)
        }
    }
}
