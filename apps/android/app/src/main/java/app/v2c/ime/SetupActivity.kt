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
import androidx.annotation.DrawableRes
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import app.v2c.ime.cloud.CloudSettingsSection
import app.v2c.ime.ui.LocalV2cStatus
import app.v2c.ime.ui.V2cTheme

/** The only Activity: first-run guide (microphone permission, enable and select the keyboard). */
class SetupActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            V2cTheme {
                Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
                    SetupScreen()
                }
            }
        }
    }
}

private data class SetupStatus(val micGranted: Boolean, val enabled: Boolean, val selected: Boolean) {
    val done: Int get() = listOf(micGranted, enabled, selected).count { it }
    val allDone: Boolean get() = done == STEP_COUNT
}

private const val STEP_COUNT = 3

private fun readStatus(context: Context): SetupStatus {
    val mic = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
        PackageManager.PERMISSION_GRANTED
    val imm = context.getSystemService(InputMethodManager::class.java)
    val enabled = imm.enabledInputMethodList.any { it.packageName == context.packageName }
    val current = Settings.Secure.getString(context.contentResolver, Settings.Secure.DEFAULT_INPUT_METHOD)
    val selected = current?.startsWith(context.packageName + "/") == true
    return SetupStatus(mic, enabled, selected)
}

private fun versionName(context: Context): String = try {
    context.packageManager.getPackageInfo(context.packageName, 0).versionName ?: "?"
} catch (_: PackageManager.NameNotFoundException) {
    "?"
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

    // The keyboard picker is a window over this Activity (no pause/resume): re-read when the
    // window gets focus back, otherwise step 3 stays "not done" after picking v2c.
    val windowFocused = LocalWindowInfo.current.isWindowFocused
    LaunchedEffect(windowFocused) {
        if (windowFocused) status = readStatus(context)
    }

    val micLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        status = readStatus(context)
    }
    // The first step that is not done yet gets the filled (primary) button.
    val next = when {
        !status.micGranted -> 0
        !status.enabled -> 1
        !status.selected -> 2
        else -> -1
    }

    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.TopCenter) {
        Column(
            modifier = Modifier
                .widthIn(max = 640.dp)
                .fillMaxWidth()
                // Insets outside the scroll: content never slides under the status bar or keyboard.
                .safeDrawingPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Header(status)
            ProgressCard(status)

            SectionLabel(stringResource(R.string.setup_section_steps))
            SectionCard {
                StepRow(
                    index = 0,
                    icon = R.drawable.ic_mic,
                    title = stringResource(R.string.setup_step_permission),
                    description = stringResource(R.string.setup_step_permission_desc),
                    done = status.micGranted,
                    primary = next == 0,
                    buttonLabel = stringResource(R.string.setup_button_grant),
                ) { micLauncher.launch(Manifest.permission.RECORD_AUDIO) }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                StepRow(
                    index = 1,
                    icon = R.drawable.ic_keyboard,
                    title = stringResource(R.string.setup_step_enable),
                    description = stringResource(R.string.setup_step_enable_desc),
                    done = status.enabled,
                    primary = next == 1,
                    buttonLabel = stringResource(R.string.setup_button_enable),
                ) { context.startActivity(Intent(Settings.ACTION_INPUT_METHOD_SETTINGS)) }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                StepRow(
                    index = 2,
                    icon = R.drawable.ic_swap,
                    title = stringResource(R.string.setup_step_switch),
                    description = stringResource(R.string.setup_step_switch_desc),
                    done = status.selected,
                    primary = next == 2,
                    buttonLabel = stringResource(R.string.setup_button_switch),
                ) { context.getSystemService(InputMethodManager::class.java).showInputMethodPicker() }
            }

            SectionLabel(stringResource(R.string.cloud_section))
            CloudSettingsSection()

            SectionLabel(stringResource(R.string.setup_section_try))
            TryCard(status.allDone)

            SectionLabel(stringResource(R.string.setup_section_howto))
            SectionCard {
                Tip(R.drawable.ic_touch, stringResource(R.string.setup_tip_tap))
                Tip(R.drawable.ic_translate, stringResource(R.string.setup_tip_translate))
                Tip(R.drawable.ic_swap, stringResource(R.string.setup_tip_source))
                Tip(R.drawable.ic_wifi, stringResource(R.string.setup_tip_network))
                Tip(R.drawable.ic_download, stringResource(R.string.setup_tip_local))
                Tip(R.drawable.ic_wifi, stringResource(R.string.setup_tip_cloud))
            }

            Text(
                stringResource(R.string.setup_footer, versionName(context)),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
        }
    }
}

@Composable
private fun Header(status: SetupStatus) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Image(
            painter = painterResource(R.drawable.v2c_logo),
            contentDescription = null,
            modifier = Modifier.size(48.dp),
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(stringResource(R.string.setup_title), style = MaterialTheme.typography.headlineSmall)
            Text(
                stringResource(R.string.setup_subtitle),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        val s = LocalV2cStatus.current
        if (status.allDone) {
            StatusPill(stringResource(R.string.setup_status_ready), s.successContainer, s.success)
        } else {
            StatusPill(
                stringResource(R.string.setup_status_left, STEP_COUNT - status.done),
                s.warningContainer,
                s.warning,
            )
        }
    }
}

@Composable
private fun StatusPill(text: String, container: Color, content: Color) {
    Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        color = content,
        modifier = Modifier
            .background(container, CircleShape)
            .padding(horizontal = 12.dp, vertical = 6.dp),
    )
}

@Composable
private fun ProgressCard(status: SetupStatus) {
    Surface(
        shape = MaterialTheme.shapes.large,
        color = MaterialTheme.colorScheme.primaryContainer,
        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    stringResource(R.string.setup_progress),
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    stringResource(R.string.setup_progress_count, status.done, STEP_COUNT),
                    style = MaterialTheme.typography.titleMedium,
                )
            }
            LinearProgressIndicator(
                progress = { status.done / STEP_COUNT.toFloat() },
                modifier = Modifier.fillMaxWidth().height(8.dp),
                color = MaterialTheme.colorScheme.primary,
                trackColor = MaterialTheme.colorScheme.surfaceContainerLowest,
                drawStopIndicator = {},
            )
            if (status.allDone) {
                Text(stringResource(R.string.setup_all_done), style = MaterialTheme.typography.bodyMedium)
            }
        }
    }
}

@Composable
private fun SectionLabel(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        // Pulled toward its card: the column spacing (16 dp) separates sections, not labels.
        modifier = Modifier.padding(start = 4.dp).offset(y = 8.dp),
    )
}

@Composable
private fun SectionCard(content: @Composable () -> Unit) {
    Surface(
        shape = MaterialTheme.shapes.large,
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column { content() }
    }
}

@Composable
private fun StepRow(
    index: Int,
    @DrawableRes icon: Int,
    title: String,
    description: String,
    done: Boolean,
    primary: Boolean,
    buttonLabel: String,
    onClick: () -> Unit,
) {
    val status = LocalV2cStatus.current
    Row(
        modifier = Modifier.fillMaxWidth().padding(16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        // Leading badge: check when done, the step's icon otherwise (highlighted for the next step).
        val (badgeBg, badgeFg) = when {
            done -> status.successContainer to status.success
            primary -> MaterialTheme.colorScheme.primary to MaterialTheme.colorScheme.onPrimary
            else -> MaterialTheme.colorScheme.surfaceContainerHighest to MaterialTheme.colorScheme.onSurfaceVariant
        }
        Box(
            modifier = Modifier.size(40.dp).background(badgeBg, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                painter = painterResource(if (done) R.drawable.ic_check else icon),
                contentDescription = null,
                tint = badgeFg,
                modifier = Modifier.size(22.dp),
            )
        }
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                "${index + 1}. $title",
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Text(
                description,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        when {
            done -> Text(
                stringResource(R.string.setup_done),
                style = MaterialTheme.typography.labelLarge,
                color = status.success,
            )
            primary -> Button(onClick = onClick) { Text(buttonLabel) }
            else -> FilledTonalButton(onClick = onClick) { Text(buttonLabel) }
        }
    }
}

@Composable
private fun TryCard(ready: Boolean) {
    SectionCard {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (ready) {
                var text by rememberSaveable { mutableStateOf("") }
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    modifier = Modifier.fillMaxWidth(),
                    placeholder = { Text(stringResource(R.string.setup_try_hint)) },
                    minLines = 3,
                    shape = MaterialTheme.shapes.medium,
                )
            } else {
                Text(
                    stringResource(R.string.setup_try_locked),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun Tip(@DrawableRes icon: Int, text: String) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(14.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Box(
            modifier = Modifier.size(32.dp).background(MaterialTheme.colorScheme.secondaryContainer, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                painter = painterResource(icon),
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSecondaryContainer,
                modifier = Modifier.size(18.dp),
            )
        }
        Text(
            text,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.weight(1f).padding(top = 6.dp),
            fontWeight = FontWeight.Normal,
        )
    }
}
