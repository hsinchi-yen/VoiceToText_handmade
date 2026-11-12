package app.v2c.ime.asr.local.debug

import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import app.v2c.ime.asr.local.Cer
import app.v2c.ime.asr.local.DownloadState
import app.v2c.ime.asr.local.LocalModelDownloader
import app.v2c.ime.asr.local.LocalModelSpec
import app.v2c.ime.asr.local.LocalModels
import app.v2c.ime.asr.local.SherpaLocalAsr
import app.v2c.ime.asr.local.WavReader
import app.v2c.ime.asr.local.ZhTwConverter
import app.v2c.ime.ffi.UniffiV2cFfi
import app.v2c.ime.ffi.V2cFfiException
import app.v2c.ime.ui.LocalV2cStatus
import app.v2c.ime.ui.V2cTheme
import java.io.File
import java.util.Locale

/**
 * Debug-only model comparison tool (T2.6). Not part of release builds.
 *
 * Workflow (all on the phone, airplane mode is fine once the models are downloaded):
 * 1. `adb push tests/fixtures/audio/. /sdcard/Android/data/app.v2c.ime/files/compare/`
 *    (pairs of `name.wav` + `name.txt` with the expected text, or `name.raw.txt` which takes
 *    precedence; 16-bit PCM WAV).
 * 2. Open this activity, press "下載" for a model (needs network once), then "執行".
 * 3. Results appear on screen and in `files/compare/result-<model>.csv` (pull it with adb).
 *
 * Output is numbers only (CER, latency, real-time factor). Recognized text is never logged,
 * written or shown, and the expected text is only used to compute the CER.
 */
class CompareActivity : ComponentActivity() {
    private val downloaders = LinkedHashMap<String, LocalModelDownloader>()

    /** Per model id: download state, "running" flag, last summary line (numbers only). */
    private val states = mutableStateMapOf<String, DownloadState>()
    private val running = mutableStateMapOf<String, Boolean>()
    private val summaries = mutableStateMapOf<String, String>()
    private val message = mutableStateOf("")

    private val compareDir: File get() = File(getExternalFilesDir(null), "compare")
    private val modelsRoot: File get() = File(filesDir, "models")

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        for (spec in LocalModels.ALL) {
            val d = LocalModelDownloader(spec, modelsRoot)
            downloaders[spec.id] = d
            states[spec.id] = d.state
            d.addListener { s -> runOnUiThread { states[spec.id] = s } }
            File(compareDir, "result-${spec.id}.csv").takeIf { it.isFile }?.readLines()?.lastOrNull()
                ?.let { summaries[spec.id] = it }
        }
        val wavs = compareDir.listFiles { f -> f.extension.equals("wav", true) }?.size ?: 0
        message.value = "錄音檔：$wavs 個（${compareDir.path}）"
        setContent { V2cTheme { Screen() } }
    }

    @Composable
    private fun Screen() {
        Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
            Box(contentAlignment = Alignment.TopCenter) {
                Column(
                    modifier = Modifier
                        .widthIn(max = 720.dp)
                        .fillMaxWidth()
                        .safeDrawingPadding()
                        .verticalScroll(rememberScrollState())
                        .padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Text("本機模型比較", style = MaterialTheme.typography.headlineSmall)
                    Text(
                        "只輸出字錯率與延遲，不顯示、不記錄辨識文字。debug 版限定。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        message.value,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    for (spec in LocalModels.ALL) ModelCard(spec)
                }
            }
        }
    }

    @Composable
    private fun ModelCard(spec: LocalModelSpec) {
        val d = downloaders.getValue(spec.id)
        val state = states[spec.id] ?: DownloadState.NotDownloaded
        val busy = running[spec.id] == true
        Surface(
            shape = MaterialTheme.shapes.large,
            color = MaterialTheme.colorScheme.surfaceContainerLow,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(spec.displayName, style = MaterialTheme.typography.titleMedium)
                        Text(
                            "${spec.totalMegabytes} MB",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    StatePill(state, busy)
                }
                if (state is DownloadState.Downloading) {
                    LinearProgressIndicator(
                        progress = { state.fraction },
                        modifier = Modifier.fillMaxWidth().height(6.dp),
                        drawStopIndicator = {},
                    )
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (state == DownloadState.Ready) {
                        Button(onClick = { runCompare(spec) }, enabled = !busy) { Text("執行") }
                        OutlinedButton(onClick = { d.delete() }, enabled = !busy) { Text("刪除") }
                    } else if (state is DownloadState.Downloading || state == DownloadState.Verifying) {
                        OutlinedButton(onClick = { d.cancel() }) { Text("取消") }
                    } else {
                        FilledTonalButton(onClick = { d.start() }) { Text("下載") }
                    }
                }
                summaries[spec.id]?.let { summary ->
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    Text(
                        summary.replace(' ', '\n'),
                        style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                }
            }
        }
    }

    @Composable
    private fun StatePill(state: DownloadState, busy: Boolean) {
        val s = LocalV2cStatus.current
        val c = MaterialTheme.colorScheme
        val (text, bg, fg) = when {
            busy -> Triple("執行中…", c.primaryContainer, c.onPrimaryContainer)
            state == DownloadState.Ready -> Triple("已下載", s.successContainer, s.success)
            state is DownloadState.Downloading ->
                Triple("${(state.fraction * 100).toInt()}%", c.primaryContainer, c.onPrimaryContainer)
            state == DownloadState.Verifying -> Triple("檢查中", c.primaryContainer, c.onPrimaryContainer)
            state is DownloadState.Failed -> Triple("失敗 ${state.reason}", c.errorContainer, c.onErrorContainer)
            else -> Triple("未下載", c.surfaceContainerHighest, c.onSurfaceVariant)
        }
        Pill(text, bg, fg)
    }

    @Composable
    private fun Pill(text: String, bg: Color, fg: Color) {
        Text(
            text,
            style = MaterialTheme.typography.labelMedium,
            color = fg,
            modifier = Modifier.background(bg, CircleShape).padding(horizontal = 12.dp, vertical = 6.dp),
        )
    }

    private fun runCompare(spec: LocalModelSpec) {
        val d = downloaders.getValue(spec.id)
        if (!d.isReady()) return
        running[spec.id] = true
        Thread {
            val summary = try {
                compare(spec, d)
            } catch (e: Exception) {
                "error=${e.javaClass.simpleName}"
            }
            runOnUiThread {
                summaries[spec.id] = summary
                running[spec.id] = false
            }
        }.start()
    }

    /** Runs every WAV through the model; returns the summary line (numbers only). */
    private fun compare(spec: LocalModelSpec, d: LocalModelDownloader): String {
        val wavs = compareDir.listFiles { f -> f.extension.equals("wav", true) }?.sortedBy { it.name }.orEmpty()
        if (wavs.isEmpty()) return "no_wav_files"
        // Same zh_tw conversion as the keyboard, so the CER is not inflated by simplified characters.
        val ffi = UniffiV2cFfi()
        val asr = SherpaLocalAsr(
            d,
            ZhTwConverter { text ->
                try {
                    ffi.toZhTw(text)
                } catch (_: V2cFfiException) {
                    text
                }
            },
        )
        val loadStart = System.nanoTime()
        asr.transcribe(ShortArray(1600), 16000) // warm-up: loads the model, excluded from per-file latency
        val loadMs = (System.nanoTime() - loadStart) / 1_000_000
        val rows = StringBuilder("file,audio_s,latency_ms,rtf,cer,ref_chars,errors\n")
        val scores = ArrayList<Cer.Score>()
        val latencies = ArrayList<Long>()
        var totalAudio = 0.0
        for (wav in wavs) {
            // Prefer <name>.raw.txt (what was actually said) over <name>.txt, which may be polished text.
            val raw = File(wav.parentFile, wav.nameWithoutExtension + ".raw.txt")
            val expected = if (raw.isFile) raw else File(wav.parentFile, wav.nameWithoutExtension + ".txt")
            if (!expected.isFile) continue
            val audio = WavReader.read(wav)
            val t0 = System.nanoTime()
            val hyp = asr.transcribe(audio.pcm, audio.rate)
            val ms = (System.nanoTime() - t0) / 1_000_000
            val score = Cer.score(expected.readText(), hyp)
            scores += score
            latencies += ms
            totalAudio += audio.seconds
            rows.append(
                String.format(
                    Locale.US, "%s,%.2f,%d,%.2f,%.4f,%d,%d\n",
                    wav.nameWithoutExtension, audio.seconds, ms, ms / 1000.0 / audio.seconds,
                    score.rate, score.referenceLength, score.errors,
                ),
            )
        }
        asr.release()
        val sorted = latencies.sorted()
        val summary = String.format(
            Locale.US,
            "model=%s files=%d load_ms=%d cer=%.4f latency_ms_median=%d latency_ms_max=%d rtf=%.2f",
            spec.id, scores.size, loadMs, Cer.aggregate(scores),
            sorted.getOrElse(sorted.size / 2) { 0 }, sorted.lastOrNull() ?: 0,
            (sorted.sum() / 1000.0) / totalAudio.coerceAtLeast(0.001),
        )
        File(compareDir, "result-${spec.id}.csv").writeText(rows.toString() + summary + "\n")
        Log.i("v2c.compare", summary)
        return summary
    }
}
