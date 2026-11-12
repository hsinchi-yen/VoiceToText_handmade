package app.v2c.ime.asr.local

/** One file of a local model. [sha256] is lowercase hex and is checked before the file is used. */
data class ModelFile(
    val name: String,
    val url: String,
    val sizeBytes: Long,
    val sha256: String,
)

/** Which sherpa-onnx model family a spec belongs to (decides the recognizer config). */
enum class ModelFamily { SENSE_VOICE, PARAFORMER, WHISPER }

/**
 * A downloadable local ASR model. All values here are data (plan: model names, endpoints and
 * thresholds are config, not logic). Sizes and hashes come from the Hugging Face file listing
 * (LFS sha256) and, for tokens files, from hashing the downloaded file; see
 * docs/android-local-model.md.
 */
data class LocalModelSpec(
    val id: String,
    val displayName: String,
    val family: ModelFamily,
    val files: List<ModelFile>,
    /** Whisper only: spoken language hint passed to the decoder. */
    val language: String = "",
) {
    val totalBytes: Long get() = files.sumOf { it.sizeBytes }

    /** Size shown in the prompt, in MB (decimal, rounded). */
    val totalMegabytes: Int get() = ((totalBytes + 500_000L) / 1_000_000L).toInt()

    fun file(name: String): ModelFile = files.first { it.name == name }
}

object LocalModels {
    private const val HF = "https://huggingface.co/csukuangfj"

    /** Default candidate. Chinese/English/Japanese/Korean/Cantonese, int8. */
    val SENSE_VOICE_INT8 = LocalModelSpec(
        id = "sense-voice-int8",
        displayName = "SenseVoice-Small int8",
        family = ModelFamily.SENSE_VOICE,
        files = listOf(
            ModelFile(
                name = "model.int8.onnx",
                url = "$HF/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/resolve/main/model.int8.onnx",
                sizeBytes = 237_115_547L,
                sha256 = "12ca1a2ae7ecf3e0019ef2822307ee0b5cadc9196569e379b4c4026f8205276d",
            ),
            ModelFile(
                name = "tokens.txt",
                url = "$HF/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/resolve/main/tokens.txt",
                sizeBytes = 315_894L,
                sha256 = "f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc",
            ),
        ),
    )

    /** Comparison candidate: offline Paraformer, Chinese with some English. */
    val PARAFORMER_ZH_INT8 = LocalModelSpec(
        id = "paraformer-zh-int8",
        displayName = "Paraformer zh (2023-09-14) int8",
        family = ModelFamily.PARAFORMER,
        files = listOf(
            ModelFile(
                name = "model.int8.onnx",
                url = "$HF/sherpa-onnx-paraformer-zh-2023-09-14/resolve/main/model.int8.onnx",
                sizeBytes = 243_371_218L,
                sha256 = "f36a0433bcf096bd6d6f11b80a3ac8bed110bdca632fe0d731df8d1a84475945",
            ),
            ModelFile(
                name = "tokens.txt",
                url = "$HF/sherpa-onnx-paraformer-zh-2023-09-14/resolve/main/tokens.txt",
                sizeBytes = 75_756L,
                sha256 = "59aba8873a2ed1e122c25fee421e25f283b63290efbde85c1f01a853d83cb6e6",
            ),
        ),
    )

    /** Comparison candidate: multilingual Whisper small, int8 encoder and decoder. */
    val WHISPER_SMALL_INT8 = LocalModelSpec(
        id = "whisper-small-int8",
        displayName = "Whisper small int8",
        family = ModelFamily.WHISPER,
        language = "zh",
        files = listOf(
            ModelFile(
                name = "small-encoder.int8.onnx",
                url = "$HF/sherpa-onnx-whisper-small/resolve/main/small-encoder.int8.onnx",
                sizeBytes = 112_442_483L,
                sha256 = "4cbe7b22fa9026b843b60a68640c747de05bafb1a11b57edc0e66c232d9f33a9",
            ),
            ModelFile(
                name = "small-decoder.int8.onnx",
                url = "$HF/sherpa-onnx-whisper-small/resolve/main/small-decoder.int8.onnx",
                sizeBytes = 262_226_114L,
                sha256 = "acad50b5c782696e91b55914cc5ab4f756f1532f76e22aa6fc615f39fb69a8ee",
            ),
            ModelFile(
                name = "small-tokens.txt",
                url = "$HF/sherpa-onnx-whisper-small/resolve/main/small-tokens.txt",
                sizeBytes = 816_730L,
                sha256 = "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
            ),
        ),
    )

    val DEFAULT = SENSE_VOICE_INT8
    val ALL = listOf(SENSE_VOICE_INT8, PARAFORMER_ZH_INT8, WHISPER_SMALL_INT8)

    fun byId(id: String): LocalModelSpec? = ALL.firstOrNull { it.id == id }
}
