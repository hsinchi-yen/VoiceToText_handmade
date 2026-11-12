package app.v2c.ime

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.isSystemInDarkTheme
import android.os.SystemClock
import android.view.HapticFeedbackConstants
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.ui.platform.LocalView
import app.v2c.ime.keyboard.BackspaceRepeat
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay

private val KeyHeight = 56.dp
private val MicSize = 104.dp

/** Pure helper so the recording timer is easy to test. */
internal fun formatElapsed(elapsedMs: Long): String {
    val totalSeconds = (elapsedMs / 1000).coerceAtLeast(0)
    return "%d:%02d".format(totalSeconds / 60, totalSeconds % 60)
}

@Composable
fun KeyboardView(
    session: VoiceSession,
    onOpenSetup: () -> Unit,
    onInstallSpeechService: () -> Unit,
    onBackspace: (repeating: Boolean) -> Boolean = { false },
    onEnter: () -> Unit = {},
) {
    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
        Surface(
            modifier = Modifier
                .fillMaxWidth()
                // Android 15+ is edge-to-edge: keep the keys above the gesture / nav bar.
                .navigationBarsPadding(),
            color = MaterialTheme.colorScheme.surfaceContainer,
        ) {
            Column(
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                TopRow()
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    // Reserved: translate key (behaviour: T2.2 / T2.4).
                    PlaceholderKey(stringResource(R.string.key_translate), Modifier.width(88.dp))
                    MicKey(session)
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        BackspaceKey(onBackspace, Modifier.width(88.dp))
                        EnterKey(onEnter, Modifier.width(88.dp))
                    }
                }
                StatusArea(session, onOpenSetup, onInstallSpeechService)
            }
        }
    }
}

@Composable
private fun TopRow() {
    Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        // Reserved: recognition-source icon (behaviour: T2.4). Shows the current source.
        Box(
            modifier = Modifier
                .background(MaterialTheme.colorScheme.secondaryContainer, RoundedCornerShape(50))
                .padding(horizontal = 12.dp, vertical = 4.dp),
        ) {
            Text(
                text = stringResource(R.string.source_system),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSecondaryContainer,
            )
        }
    }
}

@Composable
private fun BackspaceKey(onBackspace: (repeating: Boolean) -> Boolean, modifier: Modifier = Modifier) {
    val view = LocalView.current
    val repeat = remember { BackspaceRepeat() }
    Box(
        modifier = modifier
            .height(KeyHeight)
            .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(16.dp))
            .pointerInput(onBackspace) {
                detectTapGestures(onPress = {
                    view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
                    onBackspace(false)
                    coroutineScope {
                        val job = launch {
                            var i = 0
                            while (true) {
                                delay(repeat.delayBeforeRepeat(i++))
                                // Stops when the input connection changed or went away.
                                if (!onBackspace(true)) break
                            }
                        }
                        try {
                            awaitRelease()
                        } finally {
                            job.cancel()
                        }
                    }
                })
            },
        contentAlignment = Alignment.Center,
    ) {
        Text(stringResource(R.string.key_backspace), color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun EnterKey(onEnter: () -> Unit, modifier: Modifier = Modifier) {
    val view = LocalView.current
    Box(
        modifier = modifier
            .height(KeyHeight)
            .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(16.dp))
            .pointerInput(onEnter) {
                detectTapGestures(
                    onPress = { view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP) },
                    // Fire on release so sliding off the key cancels it (enter may send a message).
                    onTap = { onEnter() },
                )
            },
        contentAlignment = Alignment.Center,
    ) {
        Text(stringResource(R.string.key_enter), color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun PlaceholderKey(label: String, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .height(KeyHeight)
            .alpha(0.4f)
            .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(16.dp)),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun MicKey(session: VoiceSession) {
    val recording = session.state is VoiceUiState.Recording
    val color = if (recording) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary
    Box(
        modifier = Modifier
            .size(MicSize)
            .background(color, CircleShape)
            .pointerInput(session) {
                awaitEachGesture {
                    awaitFirstDown()
                    session.onMicPress()
                    // Wait until every pointer is up, even if the finger slides off the circle.
                    // A real system cancel ends the event stream, which also counts as a release.
                    do {
                        val event = awaitPointerEvent()
                    } while (event.changes.any { it.pressed })
                    session.onMicRelease()
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        if (session.state is VoiceUiState.Recognizing) {
            CircularProgressIndicator(color = Color.White, modifier = Modifier.size(40.dp))
        } else {
            Icon(
                painter = painterResource(R.drawable.ic_mic),
                contentDescription = stringResource(R.string.desc_mic),
                tint = Color.White,
                modifier = Modifier.size(48.dp),
            )
        }
    }
}

@Composable
private fun StatusArea(
    session: VoiceSession,
    onOpenSetup: () -> Unit,
    onInstallSpeechService: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .height(64.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        when (val s = session.state) {
            is VoiceUiState.Idle ->
                Text(stringResource(R.string.status_idle), style = MaterialTheme.typography.bodyMedium)

            is VoiceUiState.Recording -> RecordingStatus(s, session)

            is VoiceUiState.Recognizing ->
                Text(stringResource(R.string.status_recognizing), style = MaterialTheme.typography.bodyMedium)

            is VoiceUiState.Failed -> {
                Text(
                    stringResource(s.kind.message),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    when (s.kind) {
                        ErrorKind.NO_PERMISSION -> Button(onClick = onOpenSetup) {
                            Text(stringResource(R.string.setup_button_grant))
                        }
                        ErrorKind.UNAVAILABLE -> Button(onClick = onInstallSpeechService) {
                            Text(stringResource(R.string.action_install_google))
                        }
                        else -> Unit
                    }
                    TextButton(onClick = session::dismissError) {
                        Text(stringResource(R.string.action_retry))
                    }
                }
            }
        }
    }
}

@Composable
private fun RecordingStatus(state: VoiceUiState.Recording, session: VoiceSession) {
    var nowMs by remember(state) { mutableLongStateOf(SystemClock.uptimeMillis()) }
    LaunchedEffect(state) {
        while (true) {
            nowMs = SystemClock.uptimeMillis()
            delay(250)
        }
    }
    val barColor = MaterialTheme.colorScheme.primary
    Canvas(modifier = Modifier.fillMaxWidth().height(28.dp)) {
        val levels = session.levels
        val slot = size.width / levels.size
        val barWidth = slot * 0.55f
        levels.forEachIndexed { i, level ->
            val h = size.height * (0.12f + 0.88f * level)
            drawRoundRect(
                color = barColor,
                topLeft = Offset(i * slot + (slot - barWidth) / 2, (size.height - h) / 2),
                size = Size(barWidth, h),
                cornerRadius = CornerRadius(barWidth / 2),
            )
        }
    }
    Text(
        text = formatElapsed(nowMs - state.startedAtMs) + "  " + stringResource(R.string.status_recording),
        style = MaterialTheme.typography.bodyMedium,
    )
}
