package app.v2c.ime.asr.local.debug

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.view.ViewGroup
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
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
import java.io.File
import java.util.Locale

/**
 * Debug-only model comparison tool (T2.6). Not part of release builds.
 *
 * Workflow (all on the phone, airplane mode is fine once the models are downloaded):
 * 1. `adb push tests/fixtures/audio/. /sdcard/Android/data/app.v2c.ime/files/compare/`
 *    (pairs of `name.wav` + `name.txt` with the expected text, or `name.raw.txt` which takes
 *    precedence; 16-bit PCM WAV).
 * 2. Open this activity, press "Download" for a model (needs network once), then "Run".
 * 3. Results appear on screen and in `files/compare/result-<model>.csv` (pull it with adb).
 *
 * Output is numbers only (CER, latency, real-time factor). Recognized text is never logged,
 * written or shown, and the expected text is only used to compute the CER.
 */
class CompareActivity : Activity() {
    private lateinit var out: TextView
    private val downloaders = LinkedHashMap<String, LocalModelDownloader>()

    private val compareDir: File get() = File(getExternalFilesDir(null), "compare")
    private val modelsRoot: File get() = File(filesDir, "models")

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(24, 160, 24, 24) } // clear the status bar (edge-to-edge)
        for (spec in LocalModels.ALL) {
            val d = LocalModelDownloader(spec, modelsRoot)
            downloaders[spec.id] = d
            d.addListener { s -> if (s !is DownloadState.Downloading) show("${spec.id}: ${describe(s)}") }
            val row = LinearLayout(this)
            row.addView(label("${spec.displayName} (${spec.totalMegabytes} MB)"))
            row.addView(button("Download") { d.start() })
            row.addView(button("Run") { runCompare(spec) })
            row.addView(button("Delete") { d.delete() }) // non-blocking: runs on the downloader thread
            root.addView(row)
        }
        out = TextView(this).apply { textSize = 12f; setTextIsSelectable(true) }
        root.addView(ScrollView(this).apply { addView(out) })
        setContentView(root, ViewGroup.LayoutParams(-1, -1))
        show("Put name.wav + name.txt in ${compareDir.path}")
    }

    private fun label(t: String) = TextView(this).apply { text = t; textSize = 12f }

    private fun button(t: String, onClick: () -> Unit) =
        Button(this).apply { text = t; setOnClickListener { onClick() } }

    private fun show(line: String) {
        runOnUiThread { out.append(line + "\n") }
    }

    private fun describe(s: DownloadState): String = when (s) {
        is DownloadState.Failed -> "failed ${s.reason}"
        else -> s.javaClass.simpleName
    }

    private fun runCompare(spec: LocalModelSpec) {
        val d = downloaders.getValue(spec.id)
        if (!d.isReady()) {
            show("${spec.id}: model not downloaded")
            return
        }
        Thread {
            try {
                compare(spec, d)
            } catch (e: Exception) {
                show("${spec.id}: error ${e.javaClass.simpleName}")
            }
        }.start()
    }

    private fun compare(spec: LocalModelSpec, d: LocalModelDownloader) {
        val wavs = compareDir.listFiles { f -> f.extension.equals("wav", true) }?.sortedBy { it.name }.orEmpty()
        if (wavs.isEmpty()) {
            show("no wav files in ${compareDir.path}")
            return
        }
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
        show(rows.toString() + summary)
    }
}
