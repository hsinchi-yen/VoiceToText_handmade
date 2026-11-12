package app.v2c.ime

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.provider.Settings
import android.view.inputmethod.InputMethodManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver

/** The only Activity: first-run guide (microphone permission, enable and select the keyboard). */
class SetupActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            MaterialTheme(
                colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme(),
            ) {
                Surface(modifier = Modifier.fillMaxSize()) {
                    SetupScreen()
                }
            }
        }
    }
}

private data class SetupStatus(val micGranted: Boolean, val enabled: Boolean, val selected: Boolean)

private fun readStatus(context: Context): SetupStatus {
    val mic = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
        PackageManager.PERMISSION_GRANTED
    val imm = context.getSystemService(InputMethodManager::class.java)
    val enabled = imm.enabledInputMethodList.any { it.packageName == context.packageName }
    val current = Settings.Secure.getString(context.contentResolver, Settings.Secure.DEFAULT_INPUT_METHOD)
    val selected = current?.startsWith(context.packageName + "/") == true
    return SetupStatus(mic, enabled, selected)
}

@Composable
private fun SetupScreen() {
    val context = LocalContext.current
    var status by remember { mutableStateOf(readStatus(context)) }

    // Re-read on every resume: the user comes back from system settings dialogs.
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) status = readStatus(context)
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    val micLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        status = readStatus(context)
    }

    Column(
        modifier = Modifier.safeDrawingPadding().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(stringResource(R.string.setup_title), style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(16.dp))

        Step(
            title = stringResource(R.string.setup_step_permission),
            done = status.micGranted,
            buttonLabel = stringResource(R.string.setup_button_grant),
        ) { micLauncher.launch(Manifest.permission.RECORD_AUDIO) }

        Step(
            title = stringResource(R.string.setup_step_enable),
            done = status.enabled,
            buttonLabel = stringResource(R.string.setup_button_enable),
        ) { context.startActivity(Intent(Settings.ACTION_INPUT_METHOD_SETTINGS)) }

        Step(
            title = stringResource(R.string.setup_step_switch),
            done = status.selected,
            buttonLabel = stringResource(R.string.setup_button_switch),
        ) { context.getSystemService(InputMethodManager::class.java).showInputMethodPicker() }

        if (status.micGranted && status.enabled && status.selected) {
            Spacer(Modifier.height(16.dp))
            Text(stringResource(R.string.setup_all_done), style = MaterialTheme.typography.bodyLarge)
        }
    }
}

@Composable
private fun Step(title: String, done: Boolean, buttonLabel: String, onClick: () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        if (done) {
            Text(stringResource(R.string.setup_done), color = MaterialTheme.colorScheme.primary)
        } else {
            Button(onClick = onClick) { Text(buttonLabel) }
        }
        Spacer(Modifier.height(8.dp))
    }
}
