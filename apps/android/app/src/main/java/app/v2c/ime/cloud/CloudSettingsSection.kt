package app.v2c.ime.cloud

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import app.v2c.ime.R
import app.v2c.ime.asr.SharedPrefsSourcePrefs
import app.v2c.ime.ffi.AsrPick
import app.v2c.ime.ffi.CloudUse
import app.v2c.ime.ffi.UniffiV2cFfi
import app.v2c.ime.net.ConnectivityNetworkValidity
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * "雲端服務" block of the setup page (plan section 8.3, simplified version of the desktop wizard):
 * pick a provider, get a key, paste it, set models, test, choose what it is used for.
 *
 * Privacy: the typed key lives only in `remember` state (not in the saved instance state), is
 * shown masked, and goes straight to [CloudRepository] / the FFI. Nothing here logs.
 */
@Composable
fun CloudSettingsSection() {
    val context = LocalContext.current
    val repo = remember { CloudRepository.create(context) }
    val ffi = remember { UniffiV2cFfi() }
    val runtime = remember { CloudRuntime(repo, ffi, ConnectivityNetworkValidity(context)) }
    val scope = rememberCoroutineScope()
    var version by remember { mutableIntStateOf(0) }
    var editing by remember { mutableStateOf<CloudKind?>(null) }

    // Opening the setup page loads settings and keys into the FFI (changes apply from the next sentence).
    LaunchedEffect(Unit) { withContext(Dispatchers.IO) { runtime.reload() } }

    val services = remember(version) { repo.services() }

    Surface(
        shape = MaterialTheme.shapes.large,
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column {
            Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.cloud_intro), style = MaterialTheme.typography.bodyMedium)
                Text(
                    stringResource(R.string.cloud_warning),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
                Text(
                    stringResource(R.string.cloud_one_per_use),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            CloudKind.entries.forEach { kind ->
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                val saved = services.firstOrNull { it.kind == kind }
                ProviderRow(kind, saved, open = editing == kind) {
                    editing = if (editing == kind) null else kind
                }
                if (editing == kind) {
                    ProviderEditor(
                        kind = kind,
                        saved = saved,
                        repo = repo,
                        savedKeyHint = remember(version) { CloudSetupLogic.maskedKey(repo.lastFour(kind)) },
                        onTest = { draft, key ->
                            withContext(Dispatchers.IO) {
                                try {
                                    CloudSetupLogic.testDraft(ffi, draft, key, repo.snapshot())
                                } finally {
                                    // The draft stays in Rust memory after a test; restore the saved set.
                                    runtime.reload()
                                }
                            }
                        },
                        onSave = { draft, key ->
                            scope.launch {
                                withContext(Dispatchers.IO) {
                                    repo.save(draft, key)
                                    runtime.reload()
                                    resetPickIfCloudGone(context, repo)
                                }
                                version++
                            }
                        },
                        onRemove = {
                            scope.launch {
                                withContext(Dispatchers.IO) {
                                    repo.remove(kind)
                                    runtime.reload()
                                    resetPickIfCloudGone(context, repo)
                                }
                                editing = null
                                version++
                            }
                        },
                    )
                }
            }
        }
    }
}

/** A forced "cloud" source makes no sense once no recognition service is left. */
private fun resetPickIfCloudGone(context: android.content.Context, repo: CloudRepository) {
    if (repo.services().none { CloudUse.ASR in it.uses }) {
        val prefs = SharedPrefsSourcePrefs(context)
        if (prefs.pick == AsrPick.CLOUD) prefs.pick = AsrPick.AUTO
    }
}

@Composable
private fun ProviderRow(kind: CloudKind, saved: CloudServiceSettings?, open: Boolean, onToggle: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(kind.label, style = MaterialTheme.typography.titleMedium)
            Text(
                if (saved == null) {
                    stringResource(R.string.cloud_status_not_configured)
                } else {
                    stringResource(R.string.cloud_status_configured) + " · " + usesText(saved)
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        when {
            open -> TextButton(onClick = onToggle) { Text(stringResource(R.string.cloud_close)) }
            saved == null -> FilledTonalButton(onClick = onToggle) { Text(stringResource(R.string.cloud_configure)) }
            else -> OutlinedButton(onClick = onToggle) { Text(stringResource(R.string.cloud_configure)) }
        }
    }
}

@Composable
private fun usesText(s: CloudServiceSettings): String = listOfNotNull(
    if (CloudUse.ASR in s.uses) stringResource(R.string.cloud_use_asr) else null,
    if (CloudUse.TRANSLATE in s.uses) stringResource(R.string.cloud_use_translate) else null,
).joinToString("、")

@Composable
private fun ProviderEditor(
    kind: CloudKind,
    saved: CloudServiceSettings?,
    repo: CloudRepository,
    savedKeyHint: String?,
    onTest: suspend (CloudServiceSettings, String?) -> List<UseTestResult>,
    onSave: (CloudServiceSettings, String?) -> Unit,
    onRemove: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val start = saved ?: CloudServiceSettings.suggested(kind)

    // Plain `remember`: the typed key must never reach the saved instance state.
    var key by remember(kind) { mutableStateOf("") }
    var model by remember(kind) { mutableStateOf(start.model) }
    var asrModel by remember(kind) { mutableStateOf(start.asrModel) }
    var baseUrl by remember(kind) { mutableStateOf(start.baseUrl) }
    var uses by remember(kind) { mutableStateOf(start.uses) }
    var testing by remember(kind) { mutableStateOf(false) }
    var results by remember(kind) { mutableStateOf<List<UseTestResult>>(emptyList()) }
    var problem by remember(kind) { mutableStateOf<DraftProblem?>(null) }
    var savedNote by remember(kind) { mutableStateOf(false) }

    fun draft() = CloudServiceSettings(kind, model, asrModel, baseUrl, CloudSetupLogic.normalizeUses(kind, uses))

    fun check(): Boolean {
        problem = CloudSetupLogic.validate(draft(), keyAvailable = key.isNotBlank() || repo.hasKey(kind))
        return problem == null
    }

    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        // Step: how to get a key.
        Text(
            stringResource(if (kind == CloudKind.CUSTOM) R.string.cloud_get_key_custom else R.string.cloud_get_key_help),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        kind.keyPageUrl?.let { url ->
            OutlinedButton(onClick = {
                try {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
                } catch (_: ActivityNotFoundException) {
                    // No browser installed; the URL is not secret but there is nowhere to open it.
                }
            }) { Text(stringResource(R.string.cloud_open_key_page)) }
        }

        if (kind == CloudKind.CUSTOM) {
            OutlinedTextField(
                value = baseUrl,
                onValueChange = { baseUrl = it },
                label = { Text(stringResource(R.string.cloud_base_url_label)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        // Step: paste the key (masked; only the last four characters of a saved key are shown).
        OutlinedTextField(
            value = key,
            onValueChange = { key = it },
            label = { Text(stringResource(R.string.cloud_key_label)) },
            placeholder = {
                Text(
                    when {
                        savedKeyHint != null -> stringResource(R.string.cloud_key_saved, savedKeyHint.takeLast(4))
                        !kind.keyRequired -> stringResource(R.string.cloud_key_optional)
                        else -> ""
                    },
                )
            },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth(),
        )
        // Step: models (pre-filled with the suggested ones).
        OutlinedTextField(
            value = model,
            onValueChange = { model = it },
            label = { Text(stringResource(R.string.cloud_model_label)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        if (kind.supportsAsr) {
            OutlinedTextField(
                value = asrModel,
                onValueChange = { asrModel = it },
                label = { Text(stringResource(R.string.cloud_asr_model_label)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        // Step: what it is used for. Claude cannot take audio.
        Text(stringResource(R.string.cloud_uses_label), style = MaterialTheme.typography.labelLarge)
        UseCheck(R.string.cloud_use_asr, enabled = kind.supportsAsr, checked = CloudUse.ASR in uses) {
            uses = if (it) uses + CloudUse.ASR else uses - CloudUse.ASR
        }
        UseCheck(R.string.cloud_use_translate, enabled = true, checked = CloudUse.TRANSLATE in uses) {
            uses = if (it) uses + CloudUse.TRANSLATE else uses - CloudUse.TRANSLATE
        }
        if (!kind.supportsAsr) {
            Text(
                stringResource(R.string.cloud_use_asr_only_note),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        problem?.let {
            Text(stringResource(it.message), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
        }
        results.forEach { r ->
            val name = stringResource(if (r.use == CloudUse.ASR) R.string.cloud_use_asr else R.string.cloud_use_translate)
            if (r.error == null) {
                Text(
                    stringResource(R.string.cloud_test_ok, name, r.latencyMs),
                    style = MaterialTheme.typography.bodySmall,
                )
            } else {
                Text(
                    stringResource(R.string.cloud_test_fail, name, stringResource(CloudSetupLogic.testErrorMessage(r.error))),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
        }
        if (savedNote) Text(stringResource(R.string.cloud_saved), style = MaterialTheme.typography.bodySmall)

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedButton(
                enabled = !testing,
                onClick = {
                    if (check()) {
                        testing = true
                        results = emptyList()
                        savedNote = false
                        val d = draft()
                        val typed = key
                        scope.launch {
                            results = onTest(d, typed)
                            testing = false
                        }
                    }
                },
            ) { Text(stringResource(if (testing) R.string.cloud_testing else R.string.cloud_test)) }
            Button(
                enabled = !testing,
                onClick = {
                    if (check()) {
                        onSave(draft(), key)
                        key = ""
                        savedNote = true
                    }
                },
            ) { Text(stringResource(R.string.cloud_save)) }
            if (saved != null) {
                TextButton(onClick = onRemove) { Text(stringResource(R.string.cloud_remove)) }
            }
        }
    }
}

@Composable
private fun UseCheck(label: Int, enabled: Boolean, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Checkbox(checked = checked && enabled, onCheckedChange = onChange, enabled = enabled)
        Text(stringResource(label), style = MaterialTheme.typography.bodyMedium)
    }
}
