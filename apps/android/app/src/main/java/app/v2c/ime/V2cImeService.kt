package app.v2c.ime

import android.content.Intent
import android.inputmethodservice.InputMethodService
import android.net.Uri
import android.view.View
import androidx.compose.ui.platform.ComposeView
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

    override val lifecycle: Lifecycle get() = lifecycleRegistry
    override val savedStateRegistry: SavedStateRegistry get() = savedStateController.savedStateRegistry
    override val viewModelStore: ViewModelStore get() = store

    override fun onCreate() {
        super.onCreate()
        savedStateController.performAttach()
        savedStateController.performRestore(null)
        lifecycleRegistry.currentState = Lifecycle.State.CREATED
        session = VoiceSession(this) { text -> currentInputConnection?.commitText(text, 1) }
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
                    onOpenSetup = ::openSetup,
                    onInstallSpeechService = ::openSpeechServiceInStore,
                )
            }
        }
    }

    override fun onWindowShown() {
        super.onWindowShown()
        lifecycleRegistry.currentState = Lifecycle.State.RESUMED
    }

    override fun onWindowHidden() {
        super.onWindowHidden()
        session.destroy()
        lifecycleRegistry.currentState = Lifecycle.State.STARTED
    }

    override fun onDestroy() {
        session.destroy()
        lifecycleRegistry.currentState = Lifecycle.State.DESTROYED
        store.clear()
        super.onDestroy()
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
        const val SPEECH_SERVICE_STORE_URI = "market://details?id=com.google.android.googlequicksearchbox"
    }
}
