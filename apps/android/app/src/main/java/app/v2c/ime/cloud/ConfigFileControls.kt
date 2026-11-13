package app.v2c.ime.cloud

import android.content.Context
import android.net.Uri
import android.util.Log
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import app.v2c.ime.R
import app.v2c.ime.ffi.ConfigFilePreview
import app.v2c.ime.ffi.CloudUse
import app.v2c.ime.ffi.ImportStatus
import app.v2c.ime.ffi.V2cFfi
import app.v2c.ime.ffi.V2cFfiException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val TAG = "v2c-config"

/** What the dialog shows: a preview to confirm, or why the file was refused. */
private sealed interface ImportDialog {
    data class Preview(val preview: ConfigFilePreview) : ImportDialog
    data class Refused(val message: Int, val line: Int?) : ImportDialog
}

/**
 * "匯入／匯出設定" buttons of the cloud block (plan section 14.3, Android). Files go through the
 * system picker (`ACTION_OPEN_DOCUMENT` / `ACTION_CREATE_DOCUMENT`), so no storage permission is
 * needed. The file text is never kept after the preview, and nothing logs content or keys: only
 * counts, an error code and the elapsed time.
 *
 * [onApplied] runs on the IO thread after the import was written (the caller reloads the FFI).
 * [onChanged] runs on the main thread afterwards (refresh the list).
 */
@Composable
fun ConfigFileControls(
    repo: CloudRepository,
    ffi: V2cFfi,
    onApplied: () -> Unit,
    onChanged: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var dialog by remember { mutableStateOf<ImportDialog?>(null) }
    var note by remember { mutableStateOf<Pair<Int, List<Any>>?>(null) }

    val opener = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            note = null
            scope.launch { dialog = withContext(Dispatchers.IO) { loadPreview(context, ffi, repo, uri) } }
        }
    }
    val creator = rememberLauncherForActivityResult(
        // Generic type: a text/* type could make the provider append ".txt" to the name.
        ActivityResultContracts.CreateDocument("application/octet-stream"),
    ) { uri ->
        if (uri != null) {
            scope.launch {
                val result = withContext(Dispatchers.IO) { writeExport(context, ffi, repo, uri) }
                note = when {
                    result == null -> R.string.cfg_export_failed to emptyList()
                    result.second > 0 -> R.string.cfg_export_done_skipped to listOf(result.first, result.second)
                    else -> R.string.cfg_export_done to listOf(result.first)
                }
            }
        }
    }

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.cfg_intro), style = MaterialTheme.typography.bodySmall)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onClick = { opener.launch(arrayOf("*/*")) }) { Text(stringResource(R.string.cfg_import)) }
            OutlinedButton(onClick = { creator.launch(ConfigFileIo.defaultFileName()) }) {
                Text(stringResource(R.string.cfg_export))
            }
        }
        note?.let { (id, args) -> Text(stringResource(id, *args.toTypedArray()), style = MaterialTheme.typography.bodySmall) }
    }

    when (val d = dialog) {
        null -> Unit
        is ImportDialog.Refused -> AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text(stringResource(R.string.cfg_refused_title)) },
            text = {
                Text(
                    stringResource(d.message) +
                        (d.line?.let { "\n" + stringResource(R.string.cfg_error_line, it) } ?: ""),
                )
            },
            confirmButton = { TextButton(onClick = { dialog = null }) { Text(stringResource(R.string.cfg_close)) } },
        )
        is ImportDialog.Preview -> PreviewDialog(
            preview = d.preview,
            repo = repo,
            onDismiss = { dialog = null },
            onApply = { keys ->
                dialog = null
                scope.launch {
                    val result = withContext(Dispatchers.IO) {
                        val started = System.nanoTime()
                        val r = ConfigFileIo.apply(repo, d.preview, keys)
                        onApplied()
                        Log.i(TAG, "config applied saved=${r.saved} failed=${r.failed.size} ms=${(System.nanoTime() - started) / 1_000_000}")
                        r
                    }
                    note = if (result.failed.isEmpty()) {
                        R.string.cfg_applied to listOf(result.saved)
                    } else {
                        R.string.cloud_save_failed to emptyList()
                    }
                    onChanged()
                }
            },
        )
    }
}

/** Reads the picked file and asks Rust for the preview. Blocking. */
private fun loadPreview(context: Context, ffi: V2cFfi, repo: CloudRepository, uri: Uri): ImportDialog {
    val started = System.nanoTime()
    val text = try {
        context.contentResolver.openInputStream(uri)?.use { ConfigFileIo.readLimited(it) }
    } catch (_: Exception) {
        null
    }
    if (text == null) {
        Log.i(TAG, "config read failed")
        return ImportDialog.Refused(R.string.cfg_err_too_large, null)
    }
    val preview = try {
        ConfigFileIo.preview(ffi, repo, text)
    } catch (e: V2cFfiException) {
        Log.i(TAG, "config preview failed code=${e.code}")
        return ImportDialog.Refused(R.string.cfg_err_other, null)
    }
    val ms = (System.nanoTime() - started) / 1_000_000
    val code = preview.errorCode
    Log.i(TAG, "config preview services=${preview.services.size} skipped=${preview.skipped.size} error=${code ?: "none"} ms=$ms")
    return if (code != null) {
        ImportDialog.Refused(ConfigFileIo.errorMessage(code), preview.errorLine)
    } else {
        ImportDialog.Preview(preview)
    }
}

/** Writes the export; returns (services written, services left out) or null on failure. */
private fun writeExport(context: Context, ffi: V2cFfi, repo: CloudRepository, uri: Uri): Pair<Int, Int>? {
    val started = System.nanoTime()
    return try {
        val export = ConfigFileIo.export(ffi, repo)
        context.contentResolver.openOutputStream(uri, "wt")?.use { it.write(export.text.toByteArray(Charsets.UTF_8)) }
            ?: return null
        val count = repo.services().size - export.skipped.size
        Log.i(TAG, "config exported services=$count skipped=${export.skipped.size} ms=${(System.nanoTime() - started) / 1_000_000}")
        count to export.skipped.size
    } catch (e: Exception) {
        Log.i(TAG, "config export failed type=${e.javaClass.simpleName}")
        null
    }
}

@Composable
private fun PreviewDialog(
    preview: ConfigFilePreview,
    repo: CloudRepository,
    onDismiss: () -> Unit,
    onApply: (Map<String, String>) -> Unit,
) {
    // Plain `remember`: typed keys must never reach the saved instance state.
    val keys = remember(preview) { mutableStateMapOf<String, String>() }
    val changes = remember(preview) { ConfigFileIo.hasChanges(repo, preview) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(preview.title ?: stringResource(R.string.cfg_preview_title)) },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                preview.exportedFrom?.let {
                    Text(stringResource(R.string.cfg_from, it), style = MaterialTheme.typography.bodySmall)
                }
                if (preview.services.isEmpty()) {
                    Text(stringResource(R.string.cfg_nothing), style = MaterialTheme.typography.bodyMedium)
                }
                preview.services.forEach { s ->
                    val kind = CloudKind.entries.first { it.serviceId == s.config.id }
                    val uses = listOfNotNull(
                        if (CloudUse.ASR in s.config.uses) stringResource(R.string.cloud_use_asr) else null,
                        if (CloudUse.TRANSLATE in s.config.uses) stringResource(R.string.cloud_use_translate) else null,
                    ).joinToString("、")
                    val status = stringResource(
                        when (s.status) {
                            ImportStatus.ADDED -> R.string.cfg_status_added
                            ImportStatus.UPDATED -> R.string.cfg_status_updated
                            ImportStatus.UNCHANGED -> R.string.cfg_status_unchanged
                        },
                    )
                    Text(
                        "${kind.label}（${s.sourceId}）· $status" + if (uses.isNotEmpty()) " · $uses" else "",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    if (s.dropOldKey) {
                        Text(stringResource(R.string.cfg_key_dropped), style = MaterialTheme.typography.bodySmall)
                    }
                }
                preview.skipped.forEach { s ->
                    Text(
                        stringResource(R.string.cfg_skipped_line, s.id, stringResource(ConfigFileIo.skipMessage(s.reason))),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (preview.ignored.isNotEmpty()) {
                    val labels = preview.ignored.map { n -> ConfigFileIo.ignoredLabel(n)?.let { stringResource(it) } ?: n }
                    val names = labels.joinToString("、")
                    Text(
                        stringResource(R.string.cfg_ignored_line, names),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                ConfigFileIo.needingKeys(preview).forEach { s ->
                    val kind = CloudKind.entries.first { it.serviceId == s.config.id }
                    val typed = keys[kind.serviceId].orEmpty()
                    OutlinedTextField(
                        value = typed,
                        onValueChange = { keys[kind.serviceId] = it },
                        label = { Text(stringResource(R.string.cfg_key_for, kind.label)) },
                        supportingText = {
                            if (typed.trim().length >= 4) {
                                Text(stringResource(R.string.cfg_key_typed, typed.trim().takeLast(4)))
                            } else {
                                Text(stringResource(R.string.cfg_key_later))
                            }
                        },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(enabled = changes, onClick = { onApply(keys.toMap()) }) {
                Text(stringResource(R.string.cfg_apply))
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cfg_cancel)) } },
    )
}
