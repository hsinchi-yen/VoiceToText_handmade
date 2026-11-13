package app.v2c.ime

import android.content.Intent
import android.inputmethodservice.InputMethodService
import android.net.Uri
import android.view.View
import androidx.compose.ui.platform.ComposeView
import android.speech.SpeechRecognizer
import android.util.Log
import app.v2c.ime.asr.RecognitionRouter
import app.v2c.ime.asr.SharedPrefsSourcePrefs
import app.v2c.ime.asr.local.LocalModelDownloader
import app.v2c.ime.asr.local.LocalModels
import app.v2c.ime.asr.local.SherpaLocalAsr
import app.v2c.ime.asr.local.ZhTwConverter
import app.v2c.ime.cloud.CloudRepository
import app.v2c.ime.cloud.CloudRuntime
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.UniffiV2cFfi
import app.v2c.ime.ffi.V2cFfiException
import app.v2c.ime.keyboard.EditorKeys
import app.v2c.ime.keyboard.SourceController
import app.v2c.ime.net.ConnectivityNetworkValidity
import app.v2c.ime.net.HttpUrlConnectionProber
import app.v2c.ime.net.LanCatalog
import app.v2c.ime.net.LanDetector
import app.v2c.ime.net.NetworkWatcher
import app.v2c.ime.recorder.AudioRecordPcmRecorder
import app.v2c.ime.translate.Translator
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.util.concurrent.Executors
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.ViewModelStoreOwner
import androidx.lifecycle.setViewTreeLifecycleOwner
import androidx.lifecycle.setViewTreeViewModelStoreOwner
import androidx.savedstate.SavedStateRegistry
import androidx.savedstate.SavedStateRegistryController
import androidx.savedstate.SavedStateRegistryOwner
import androidx.savedstate.setViewTreeSavedStateRegistryOwner

/**
 * Voice-only keyboard. The service doubles as the lifecycle / saved-state owner that
 * Compose needs because an IME window has no Activity.
 */
class V2cImeService :
    InputMethodService(),
    LifecycleOwner,
    SavedStateRegistryOwner,
    ViewModelStoreOwner {

    private val lifecycleRegistry = LifecycleRegistry(this)
    private val savedStateController = SavedStateRegistryController.create(this)
    private val store = ViewModelStore()
    private lateinit var session: VoiceSession
    private lateinit var source: SourceController
    private lateinit var detector: LanDetector
    private lateinit var catalog: LanCatalog
    private lateinit var localAsr: SherpaLocalAsr
    private lateinit var networkWatcher: NetworkWatcher
    private lateinit var cloud: CloudRuntime
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val probeExecutor = Executors.newCachedThreadPool { r ->
        Thread(r, "v2c-lan-probe").apply { isDaemon = true }
    }
    private val editorKeys = EditorKeys({ currentInputConnection }, { currentInputEditorInfo })

    override val lifecycle: Lifecycle get() = lifecycleRegistry
    override val savedStateRegistry: SavedStateRegistry get() = savedStateController.savedStateRegistry
    override val viewModelStore: ViewModelStore get() = store

    override fun onCreate() {
        super.onCreate()
        savedStateController.performAttach()
        savedStateController.performRestore(null)
        lifecycleRegistry.currentState = Lifecycle.State.CREATED
        setUpRouting()
    }

    override fun onCreateInputView(): View {
        val owner = this
        // Compose looks the owners up from the root of the view tree, i.e. the IME window decor.
        window?.window?.decorView?.let { decor ->
            decor.setViewTreeLifecycleOwner(owner)
            decor.setViewTreeSavedStateRegistryOwner(owner)
            decor.setViewTreeViewModelStoreOwner(owner)
        }
        return ComposeView(this).apply {
            setViewTreeLifecycleOwner(owner)
            setViewTreeSavedStateRegistryOwner(owner)
            setViewTreeViewModelStoreOwner(owner)
            setContent {
                KeyboardView(
                    session = session,
                    source = source,
                    onOpenSetup = ::openSetup,
                    onInstallSpeechService = ::openSpeechServiceInStore,
                    onBackspace = editorKeys::backspace,
                    onEnter = editorKeys::enter,
                )
            }
        }
    }

    override fun onWindowShown() {
        super.onWindowShown()
        lifecycleRegistry.currentState = Lifecycle.State.RESUMED
        // Keyboard opened: re-probe the LAN (not on every utterance) and follow network changes.
        networkWatcher.start()
        refreshLan()
        refreshCloud()
        source.refresh()
    }

    override fun onWindowHidden() {
        super.onWindowHidden()
        networkWatcher.stop()
        session.destroy()
        lifecycleRegistry.currentState = Lifecycle.State.CREATED
    }

    override fun onDestroy() {
        networkWatcher.stop()
        session.destroy()
        source.detach()
        // release() is synchronized with transcribe(); never block the main thread on it.
        Thread({ localAsr.release() }, "v2c-local-release").start()
        probeExecutor.shutdownNow()
        scope.cancel()
        lifecycleRegistry.currentState = Lifecycle.State.DESTROYED
        store.clear()
        super.onDestroy()
    }

    private fun setUpRouting() {
        val ffi = UniffiV2cFfi()
        val log = app.v2c.ime.asr.EventLog { line -> Log.i(LOG_TAG, line) }
        val prefs = SharedPrefsSourcePrefs(this)
        catalog = LanCatalog(ffi)
        detector = LanDetector(
            catalog = catalog,
            prober = HttpUrlConnectionProber(),
            executor = probeExecutor,
            onResult = { site, ms -> Log.i(LOG_TAG, "lan detect site=$site ms=$ms") },
        )
        val downloader = LocalModelDownloader(LocalModels.DEFAULT, File(filesDir, "models"))
        localAsr = SherpaLocalAsr(
            downloader,
            ZhTwConverter { text ->
                try {
                    ffi.toZhTw(text)
                } catch (_: V2cFfiException) {
                    text
                }
            },
        )
        cloud = CloudRuntime(CloudRepository.create(this), ffi, ConnectivityNetworkValidity(this), log)
        val router = RecognitionRouter(ffi, detector, prefs, localAsr, log, cloud = cloud)
        val translator = Translator(ffi, detector, catalog, log, cloud = cloud)
        session = VoiceSession(
            context = this,
            scope = scope,
            router = router,
            recorder = AudioRecordPcmRecorder(this),
            translator = translator,
            cloud = cloud,
            onPlanChanged = { source.refresh() },
        ) { text -> currentInputConnection?.commitText(text, 1) }
        source = SourceController(router, prefs, downloader, { SpeechRecognizer.isRecognitionAvailable(this) }, cloud)
        source.attach()
        networkWatcher = NetworkWatcher(
            this,
            onChanged = {
                // System thread, only for a real network change: drop the cached LAN at once, then re-probe.
                detector.invalidate()
                scope.launch {
                    session.onLanChanged(LanSite.UNREACHABLE)
                    session.onCloudChanged()
                    source.refresh()
                }
                refreshLan()
            },
            // The system confirmed (or lost) internet access: cloud readiness changed, the LAN did not.
            onValidatedChanged = {
                scope.launch {
                    session.onCloudChanged()
                    source.refresh()
                }
            },
        )
        // Presets first (needed for the probe URLs), then the first LAN detection.
        scope.launch {
            withContext(Dispatchers.IO) {
                try {
                    val tn = assets.open(PRESET_TN).bufferedReader().use { it.readText() }
                    val rd = assets.open(PRESET_RD).bufferedReader().use { it.readText() }
                    catalog.load(tn, rd)
                } catch (e: Exception) {
                    // Without presets there is no LAN: system / local sources still work.
                    Log.w(LOG_TAG, "presets failed type=${e.javaClass.simpleName}")
                }
            }
            refreshLan()
        }
        refreshCloud()
    }

    /** Loads the cloud settings and keys into the FFI off the main thread, then refreshes the UI. */
    private fun refreshCloud() {
        scope.launch {
            withContext(Dispatchers.IO) {
                try {
                    cloud.reload()
                } catch (e: Exception) {
                    Log.w(LOG_TAG, "cloud reload failed type=${e.javaClass.simpleName}")
                }
            }
            session.onCloudChanged()
            source.refresh()
        }
    }

    /** Probes the LAN off the main thread and publishes the result to the keyboard. */
    private fun refreshLan() {
        scope.launch {
            withContext(Dispatchers.IO) { detector.refresh() }
            // Publish the cache, not refresh()'s return: an older overlapping probe must not reach the UI.
            session.onLanChanged(detector.current())
            source.refresh()
        }
    }

    private fun openSetup() {
        startActivity(
            Intent(this, SetupActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }

    private fun openSpeechServiceInStore() {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(SPEECH_SERVICE_STORE_URI))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            startActivity(intent)
        } catch (_: android.content.ActivityNotFoundException) {
            // No store app installed; nothing more the keyboard can do.
        }
    }

    private companion object {
        const val LOG_TAG = "v2c.route"
        const val PRESET_TN = "presets/TN_CUSTOM_LLM_ADDR.toml"
        const val PRESET_RD = "presets/RD_CUSTOM_LLM_ADDR.toml"
        const val SPEECH_SERVICE_STORE_URI = "market://details?id=com.google.android.googlequicksearchbox"
    }
}
