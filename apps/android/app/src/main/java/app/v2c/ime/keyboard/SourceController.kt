package app.v2c.ime.keyboard

import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import app.v2c.ime.asr.RecognitionRouter
import app.v2c.ime.asr.SelectEffect
import app.v2c.ime.asr.SourceMenuState
import app.v2c.ime.asr.SourcePrefs
import app.v2c.ime.asr.local.DownloadState
import app.v2c.ime.asr.local.LocalModelDownloader
import app.v2c.ime.cloud.CloudRouting
import app.v2c.ime.ffi.AsrPick
import app.v2c.ime.ffi.AsrPlan

/**
 * Compose-facing holder for the recognition-source chip, its inline menu and the local-model
 * download prompt. The decisions live in the pure [SourceMenuState] / [RecognitionRouter];
 * this class only mirrors them into Compose state. Main thread only (the downloader's listener
 * is bounced to the main thread). Android glue, not unit tested.
 */
class SourceController(
    private val router: RecognitionRouter,
    private val prefs: SourcePrefs,
    private val downloader: LocalModelDownloader,
    private val systemAvailable: () -> Boolean,
    private val cloud: CloudRouting = CloudRouting.None,
) {
    private val main = Handler(Looper.getMainLooper())

    var menu: SourceMenuState by mutableStateOf(SourceMenuState.load(prefs))
        private set

    /** What the next utterance would use; null until the first [refresh]. */
    var plan: AsrPlan? by mutableStateOf(null)
        private set

    var downloadState: DownloadState by mutableStateOf(downloader.state)
        private set

    /** True while the download question / progress is shown above the keys. */
    var promptVisible: Boolean by mutableStateOf(false)
        private set

    private val listener: (DownloadState) -> Unit = { st ->
        main.post {
            downloadState = st
            if (st == DownloadState.Ready) promptVisible = false
            refresh()
        }
    }

    fun attach() {
        downloader.addListener(listener)
        downloadState = downloader.state
        refresh()
    }

    fun detach() {
        downloader.removeListener(listener)
    }

    /** Recompute the chip after the LAN, the pick or the model state changed. */
    fun refresh() {
        plan = router.preview(systemAvailable())
    }

    /** Menu entries; "cloud" appears only when a recognition service is configured. */
    fun menuItems(): List<AsrPick> = SourceMenuState.itemsFor(cloud.asrConfigured)

    /** Chip text: the source in use now, with the cloud service's own name (e.g. "Gemini"). */
    fun chipText(): String = SourceMenuState.chipText(menu.pick, plan, cloud.asrLabel())

    fun openMenu() {
        menu = menu.openMenu()
        refresh()
    }

    fun dismissMenu() {
        menu = menu.dismiss()
    }

    fun select(pick: AsrPick) {
        val (next, effect) = menu.select(pick, prefs, localModelReady = downloader.isReady())
        menu = next
        router.pickChanged()
        if (effect == SelectEffect.SHOW_LOCAL_PROMPT) promptVisible = true
        refresh()
    }

    /** Shows the download question again (the "download local model" button on an error). */
    fun showPrompt() {
        promptVisible = true
    }

    fun download() {
        prefs.localPromptAsked = true
        downloader.start()
    }

    fun cancelDownload() {
        downloader.cancel()
    }

    fun decline() {
        prefs.localPromptAsked = true
        promptVisible = false
    }
}
