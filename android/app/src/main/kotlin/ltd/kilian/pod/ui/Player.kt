package ltd.kilian.pod.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Notes
import androidx.compose.material.icons.outlined.Bedtime
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.CloudQueue
import androidx.compose.material.icons.outlined.DownloadDone
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.Replay
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import ltd.kilian.pod.episode.EPISODE_TABLES
import ltd.kilian.pod.episode.loadEpisode
import ltd.kilian.pod.episode.played
import ltd.kilian.pod.player.NowPlaying
import ltd.kilian.pod.player.PlaySource
import ltd.kilian.pod.player.PlayerConnection
import ltd.kilian.pod.player.SKIP_STEPS_MS
import ltd.kilian.pod.player.SPEEDS
import ltd.kilian.pod.player.SleepTimer
import ltd.kilian.pod.player.clockLabel
import ltd.kilian.pod.player.remainingLabel
import ltd.kilian.pod.player.skipLabel
import ltd.kilian.pod.player.sleepLabel
import ltd.kilian.pod.player.speedLabel
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.writeLocal

// ==============================================================================================
// The player (wireframe M8)
// ==============================================================================================
//
// The mini player above the tabs and the full-height now-playing sheet it opens. Both are views
// over the playback service's session ([PlayerConnection]): every control is a command to the
// service, which owns playback and its writes (spec 8). The one write made here is the sheet's
// "Played" toggle, an ordinary episode write like the episode screen's.

/** The activity's connection to the playback service. */
val LocalPlayer = staticCompositionLocalOf<PlayerConnection> { error("no PlayerConnection provided") }

/** The sleep timer's choices: device-only and ephemeral. */
private val SLEEP_MINUTES = listOf(15, 30, 45, 60)

/**
 * The episode in the player, with its progress and a play/pause button. Shown from a cold start
 * too, paused at the remembered position. Tapping it or swiping it up opens the sheet.
 */
@Composable
fun MiniPlayer() {
    val player = LocalPlayer.current
    val now by player.state.collectAsState()
    val playing = now ?: return
    var sheet by rememberSaveable { mutableStateOf(false) }

    // A short upward drag opens the sheet, the way a bottom sheet's handle would.
    var dragged by remember { mutableFloatStateOf(0f) }
    val drag = rememberDraggableState { dragged += it }
    Surface(
        tonalElevation = 3.dp,
        modifier = Modifier
            .fillMaxWidth()
            .clickable { sheet = true }
            .draggable(drag, Orientation.Vertical, onDragStarted = { dragged = 0f }, onDragStopped = { if (dragged < -40f) sheet = true }),
    ) {
        Column {
            val fraction = progressOf(playing)
            if (fraction != null) ProgressBar(progress = { fraction }, Modifier.fillMaxWidth().height(2.dp))
            Row(Modifier.padding(horizontal = 12.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Artwork(playing.artworkUrl, 40.dp)
                Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
                    Text(playing.title, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(playing.podcastTitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                IconButton(onClick = { player.skipBack() }) { Icon(Icons.Outlined.Replay, "Skip back ${skipLabel(playing.skipBackMs)}") }
                IconButton(onClick = { player.toggle() }) {
                    Icon(if (playing.playing) Icons.Outlined.Pause else Icons.Outlined.PlayArrow, if (playing.playing) "Pause" else "Play")
                }
            }
        }
    }
    if (sheet) NowPlayingSheet(playing, onDismiss = { sheet = false })
}

private fun progressOf(playing: NowPlaying): Float? {
    val duration = playing.durationMs?.takeIf { it > 0 } ?: return null
    return (playing.positionMs.toFloat() / duration).coerceIn(0f, 1f)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NowPlayingSheet(playing: NowPlaying, onDismiss: () -> Unit) {
    val player = LocalPlayer.current
    val navigator = LocalNavigator.current
    val state = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = state) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 24.dp).padding(bottom = 32.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            Artwork(playing.artworkUrl, 240.dp)
            Spacer(Modifier.height(20.dp))
            Text(playing.title, style = MaterialTheme.typography.titleLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Text(
                playing.podcastTitle,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.clickable(enabled = playing.podcastId != null) {
                    onDismiss()
                    playing.podcastId?.let { navigator.open(Screen.Podcast(it)) }
                },
            )
            SourceLine(playing.source)
            Spacer(Modifier.height(16.dp))
            Scrubber(playing)
            Transport(playing)
            Spacer(Modifier.height(16.dp))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly, verticalAlignment = Alignment.CenterVertically) {
                SpeedControl(playing.speed)
                SleepControl(playing)
                PlayedToggle(playing.episodeId)
                TextButton(onClick = {
                    onDismiss()
                    navigator.open(Screen.Episode(playing.episodeId))
                }) {
                    Icon(Icons.AutoMirrored.Outlined.Notes, contentDescription = null)
                    Text(" Notes")
                }
            }
        }
    }
}

/** "Playing from download" or "Streaming". Streaming never starts a download (spec 7.4). */
@Composable
private fun SourceLine(source: PlaySource?) {
    val (icon, label) = when (source ?: return) {
        PlaySource.Download -> Icons.Outlined.DownloadDone to "Playing from download"
        PlaySource.Stream -> Icons.Outlined.CloudQueue to "Streaming"
    }
    Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, contentDescription = null, Modifier.size(16.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(" $label", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/**
 * The position slider with elapsed and remaining time. While the thumb is held it shows where
 * it is, and the seek goes out on release, so a drag is one seek and one position write.
 */
@Composable
private fun Scrubber(playing: NowPlaying) {
    val player = LocalPlayer.current
    val duration = playing.durationMs?.takeIf { it > 0 }
    var held by remember { mutableStateOf<Long?>(null) }
    val position = held ?: playing.positionMs
    Slider(
        value = if (duration == null) 0f else (position.toFloat() / duration).coerceIn(0f, 1f),
        onValueChange = { fraction -> if (duration != null) held = (fraction * duration).toLong() },
        onValueChangeFinished = {
            held?.let(player::seekTo)
            held = null
        },
        enabled = duration != null,
    )
    Row(Modifier.fillMaxWidth()) {
        Text(clockLabel(position), style = MaterialTheme.typography.labelMedium)
        Spacer(Modifier.weight(1f))
        if (duration != null) Text(remainingLabel(position, duration), style = MaterialTheme.typography.labelMedium)
    }
}

/**
 * Skip back, play/pause, skip forward. The skip buttons show the synced intervals; a long press
 * picks another, which changes the synced setting on every device.
 */
@Composable
private fun Transport(playing: NowPlaying) {
    val player = LocalPlayer.current
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly, verticalAlignment = Alignment.CenterVertically) {
        SkipButton(playing.skipBackMs, forward = false, onSkip = player::skipBack) { player.setSkipInterval(false, it) }
        FilledIconButton(onClick = { player.toggle() }, shape = CircleShape, modifier = Modifier.size(72.dp)) {
            Icon(if (playing.playing) Icons.Outlined.Pause else Icons.Outlined.PlayArrow, if (playing.playing) "Pause" else "Play", Modifier.size(40.dp))
        }
        SkipButton(playing.skipForwardMs, forward = true, onSkip = player::skipForward) { player.setSkipInterval(true, it) }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SkipButton(intervalMs: Long, forward: Boolean, onSkip: () -> Unit, onPick: (Long) -> Unit) {
    var picking by remember { mutableStateOf(false) }
    val label = skipLabel(intervalMs)
    val description = if (forward) "Skip forward $label" else "Skip back $label"
    Box {
        Column(
            Modifier
                .size(72.dp)
                .semantics { contentDescription = description }
                .combinedClickable(onClick = onSkip, onLongClick = { picking = true }, onLongClickLabel = "Change interval"),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            // Material's numbered skip glyphs fix the interval; a plain arrow, mirrored for
            // forward, with the interval under it fits every step.
            Icon(Icons.Outlined.Replay, contentDescription = null, modifier = if (forward) Modifier.graphicsLayer(scaleX = -1f) else Modifier)
            Text(label, style = MaterialTheme.typography.labelSmall)
        }
        DropdownMenu(picking, onDismissRequest = { picking = false }) {
            SKIP_STEPS_MS.forEach { step ->
                DropdownMenuItem(
                    text = { Text(skipLabel(step) + if (step == intervalMs) "  ✓" else "") },
                    onClick = {
                        picking = false
                        onPick(step)
                    },
                )
            }
        }
    }
}

/** The synced speed. Changing it here changes it on every device. */
@Composable
private fun SpeedControl(speed: Float) {
    val player = LocalPlayer.current
    var open by remember { mutableStateOf(false) }
    Box {
        TextButton(onClick = { open = true }) { Text(speedLabel(speed)) }
        DropdownMenu(open, onDismissRequest = { open = false }) {
            SPEEDS.forEach { step ->
                DropdownMenuItem(
                    text = { Text(speedLabel(step.toFloat()) + if (step.toFloat() == speed) "  ✓" else "") },
                    onClick = {
                        open = false
                        player.setSpeed(step.toFloat())
                    },
                )
            }
        }
    }
}

/** The sleep timer, with a countdown while set. */
@Composable
private fun SleepControl(playing: NowPlaying) {
    val player = LocalPlayer.current
    var open by remember { mutableStateOf(false) }
    val now = rememberNow(1_000)
    val label = sleepLabel(playing.sleepAt, playing.sleepAtEndOfEpisode, now)
    Box {
        TextButton(onClick = { open = true }) {
            Icon(Icons.Outlined.Bedtime, contentDescription = "Sleep timer")
            if (label != null) Text(" $label")
        }
        DropdownMenu(open, onDismissRequest = { open = false }) {
            SLEEP_MINUTES.forEach { minutes ->
                DropdownMenuItem(text = { Text("$minutes min") }, onClick = {
                    open = false
                    player.setSleepTimer(SleepTimer.Minutes(minutes))
                })
            }
            DropdownMenuItem(text = { Text("End of episode") }, onClick = {
                open = false
                player.setSleepTimer(SleepTimer.EndOfEpisode)
            })
            if (label != null) {
                DropdownMenuItem(text = { Text("Turn off") }, onClick = {
                    open = false
                    player.setSleepTimer(null)
                })
            }
        }
    }
}

/** Marks the episode played or unplayed, read from the store like the episode screen's button. */
@Composable
private fun PlayedToggle(episodeId: String) {
    val pod = LocalPod.current
    val scope = rememberCoroutineScope()
    val episode by remember(episodeId) { pod.database.observe(*EPISODE_TABLES) { loadEpisode(it, episodeId) } }.collectAsState(initial = null)
    val loaded = episode ?: return
    TextButton(onClick = { scope.launch { writeLocal(pod.database, pod.sync, played(loaded.id, loaded.podcastId, !loaded.played)) } }) {
        Icon(Icons.Outlined.CheckCircle, contentDescription = null, tint = if (loaded.played) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
        Text(if (loaded.played) " Played" else " Mark played")
    }
}
