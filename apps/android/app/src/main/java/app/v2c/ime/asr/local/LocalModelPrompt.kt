package app.v2c.ime.asr.local

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/** What the keyboard should show for the local model, derived from [DownloadState]. */
sealed interface PromptContent {
    /** Nothing to show (model ready, or the user already declined once). */
    data object Hidden : PromptContent

    data class Ask(val megabytes: Int) : PromptContent

    data class Progress(val fraction: Float?, val verifying: Boolean) : PromptContent

    data class Error(val reason: DownloadFailure) : PromptContent
}

/** Pure decision logic for the prompt, unit tested. */
object LocalModelPromptPolicy {
    /**
     * The "download the local model (about N MB)?" question is asked once. [alreadyAsked] is
     * remembered by the caller (a flag in app-private preferences, wired by T2.4). An unfinished
     * or failed download is always shown so it can be resumed.
     */
    fun content(state: DownloadState, spec: LocalModelSpec, alreadyAsked: Boolean): PromptContent =
        when (state) {
            DownloadState.Ready -> PromptContent.Hidden
            DownloadState.NotDownloaded ->
                if (alreadyAsked) PromptContent.Hidden else PromptContent.Ask(spec.totalMegabytes)
            is DownloadState.Downloading -> PromptContent.Progress(state.fraction, verifying = false)
            DownloadState.Verifying -> PromptContent.Progress(null, verifying = true)
            is DownloadState.Failed -> PromptContent.Error(state.reason)
        }

    /** Traditional Chinese (Taiwan) message for a failure. */
    fun errorMessage(reason: DownloadFailure): String = when (reason) {
        DownloadFailure.INTERRUPTED -> "下載中斷，請檢查網路後重試（會從中斷處繼續）"
        DownloadFailure.HASH_MISMATCH -> "檔案檢查碼不符，已刪除，請重新下載"
        DownloadFailure.BAD_RESPONSE -> "伺服器回應異常，請稍後再試"
        DownloadFailure.STORAGE -> "儲存空間不足或無法寫入，請清出空間後重試"
    }
}

/**
 * Keyboard-side prompt for the on-device model. Not yet placed in the keyboard layout (T2.4
 * does that); it only needs the current [state] and four callbacks. UI strings are inline
 * Traditional Chinese because res/ is outside this card's paths; T2.4 may move them.
 */
@Composable
fun LocalModelPrompt(
    state: DownloadState,
    spec: LocalModelSpec,
    alreadyAsked: Boolean,
    onDownload: () -> Unit,
    onCancel: () -> Unit,
    onDecline: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when (val c = LocalModelPromptPolicy.content(state, spec, alreadyAsked)) {
        PromptContent.Hidden -> Unit
        is PromptContent.Ask -> Column(modifier.fillMaxWidth().padding(12.dp), Arrangement.spacedBy(8.dp)) {
            Text("下載本機模型（約 ${c.megabytes} MB）？", style = MaterialTheme.typography.bodyLarge)
            Text("下載後可在沒有網路時辨識，語音不會離開手機。", style = MaterialTheme.typography.bodySmall)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = onDownload) { Text("下載") }
                TextButton(onClick = onDecline) { Text("稍後") }
            }
        }
        is PromptContent.Progress -> Column(modifier.fillMaxWidth().padding(12.dp), Arrangement.spacedBy(8.dp)) {
            Text(if (c.verifying) "檢查檔案中…" else "下載本機模型中…", style = MaterialTheme.typography.bodyLarge)
            val f = c.fraction
            if (f == null) {
                LinearProgressIndicator(Modifier.fillMaxWidth())
            } else {
                LinearProgressIndicator(progress = { f }, modifier = Modifier.fillMaxWidth())
                Text("${(f * 100).toInt()}%", style = MaterialTheme.typography.bodySmall)
            }
            TextButton(onClick = onCancel) { Text("取消") }
        }
        is PromptContent.Error -> Column(modifier.fillMaxWidth().padding(12.dp), Arrangement.spacedBy(8.dp)) {
            Text(LocalModelPromptPolicy.errorMessage(c.reason), style = MaterialTheme.typography.bodyMedium)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = onDownload) { Text("重試") }
                TextButton(onClick = onDecline) { Text("稍後") }
            }
        }
    }
}
