package ltd.kilian.pod.ui

import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.Animatable
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.roundToInt
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
// Phones get a mini player floating above the tabs, which opens a full-screen player sheet;
// wide layouts get one bar along the bottom instead. All three are views over the playback
// service's session ([PlayerConnection]): every control is a command to the service, which owns
// playback and its writes (spec 8). The one write made here is the sheet's "Played" toggle, an
// ordinary episode write like the episode screen's.
//
// Each surface is washed with the episode's cover ([Ambient]), the one place the interface takes
// its colour from the show; the sheet's wash is the strongest.

/** The activity's connection to the playback service. */
val LocalPlayer = staticCompositionLocalOf<PlayerConnection> { error("no PlayerConnection provided") }

/** The sleep timer's choices: device-only and ephemeral. */
private val SLEEP_MINUTES = listOf(15, 30, 45, 60)

private fun progressOf(playing: NowPlaying): Float? {
    val duration = playing.durationMs?.takeIf { it > 0 } ?: return null
    return (playing.positionMs.toFloat() / duration).coerceIn(0f, 1f)
}

// ==============================================================================================
// The mini player (phones)
// ==============================================================================================

/**
 * The episode in the player as a floating card above the tab bar: cover, title and show, and
 * play/pause, with a thin progress line along its bottom edge. Shown from a cold start too, paused
 * at the remembered position. Tapping it or swiping it up opens the sheet.
 */
@Composable
fun MiniPlayer(onOpen: () -> Unit, modifier: Modifier = Modifier) {
    val player = LocalPlayer.current
    val now by player.state.collectAsState()
    val playing = now ?: return
    val colors = podColors
    val shape = RoundedCornerShape(14.dp)

    // A short upward drag opens the sheet, the way a bottom sheet's handle would.
    var dragged by remember { mutableStateOf(0f) }
    val drag = rememberDraggableState { dragged += it }
    Box(
        modifier
            .fillMaxWidth()
            .padding(start = 8.dp, end = 8.dp, bottom = 8.dp)
            .shadow(14.dp, shape, ambientColor = Color.Black.copy(alpha = 0.35f), spotColor = Color.Black.copy(alpha = 0.35f))
            .clip(shape)
            .background(colors.surface)
            .clickable(onClickLabel = "Open player", onClick = onOpen)
            .draggable(drag, Orientation.Vertical, onDragStarted = { dragged = 0f }, onDragStopped = { if (dragged < -40f) onOpen() }),
    ) {
        Ambient(playing.artworkUrl, Modifier.matchParentSize(), extra = 0.15f)
        Row(Modifier.padding(start = 6.dp, end = 4.dp, top = 6.dp, bottom = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            Artwork(playing.artworkUrl, 44.dp, playing.podcastTitle, radius = 8.dp)
            Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
                Text(playing.title, style = PodType.rowTitle, color = colors.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(playing.podcastTitle, style = PodType.meta, color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            PodIconButton(
                if (playing.playing) PodIcons.Pause else PodIcons.Play,
                if (playing.playing) "Pause" else "Play",
                onClick = { player.toggle() },
                tint = colors.ink, iconSize = 22.dp,
            )
        }
        val fraction = progressOf(playing) ?: 0f
        Canvas(Modifier.align(Alignment.BottomStart).fillMaxWidth().padding(horizontal = 12.dp).height(2.dp)) {
            drawRoundRect(colors.accent, size = Size(size.width * fraction, size.height), cornerRadius = CornerRadius(size.height / 2))
        }
    }
}

// ==============================================================================================
// The player sheet (phones)
// ==============================================================================================

/**
 * The full player: a chevron to close, the cover as large as fits, title and show, the scrubber
 * with its clocks, skip back / play / skip forward at thumb size, and the speed, sleep timer,
 * played toggle and notes. Dragging it down or Back closes it. In landscape the cover moves to
 * the left of the controls.
 */
@Composable
fun PlayerSheet(onClose: () -> Unit) {
    val player = LocalPlayer.current
    val navigator = LocalNavigator.current
    val now by player.state.collectAsState()
    val colors = podColors
    val scope = rememberCoroutineScope()
    val playing = now ?: run {
        // Nothing loaded any more (signed out), nothing to show.
        LaunchedEffect(Unit) { onClose() }
        return
    }
    BackHandler(onBack = onClose)

    // Dragged down past a quarter of the screen it closes; short of that it springs back.
    val offset = remember { Animatable(0f) }
    val dismissAt = with(LocalDensity.current) { 160.dp.toPx() }
    val drag = rememberDraggableState { delta -> scope.launch { offset.snapTo((offset.value + delta).coerceAtLeast(0f)) } }

    fun openEpisode() {
        onClose()
        navigator.open(Screen.Episode(playing.episodeId))
    }
    fun openShow() {
        onClose()
        playing.podcastId?.let { navigator.open(Screen.Podcast(it)) }
    }

    Box(
        Modifier
            .fillMaxSize()
            .offset { IntOffset(0, offset.value.roundToInt()) }
            .background(colors.background)
            .draggable(drag, Orientation.Vertical, onDragStopped = {
                if (offset.value > dismissAt) onClose() else scope.launch { offset.animateTo(0f) }
            })
            .semantics { contentDescription = "Now playing" },
    ) {
        Ambient(playing.artworkUrl, Modifier.matchParentSize(), extra = 0.25f)
        BoxWithConstraints(Modifier.fillMaxSize().systemBarsPadding().padding(start = 24.dp, end = 24.dp, top = 4.dp, bottom = 20.dp)) {
            val landscape = maxWidth > maxHeight * 1.15f
            val height = maxHeight
            if (landscape) {
                Row(Modifier.fillMaxSize(), horizontalArrangement = Arrangement.spacedBy(32.dp), verticalAlignment = Alignment.CenterVertically) {
                    SheetArt(playing, Modifier.fillMaxHeight().widthIn(max = height))
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(18.dp)) {
                        CloseHandle(onClose, Modifier.align(Alignment.End))
                        SheetText(playing, ::openEpisode, ::openShow)
                        Scrubber(playing, big = true)
                        Controls(playing, big = true)
                        SheetExtras(playing, onNotes = ::openEpisode)
                    }
                }
            } else {
                Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(20.dp)) {
                    CloseHandle(onClose, Modifier.align(Alignment.CenterHorizontally))
                    SheetArt(playing, Modifier.weight(1f).fillMaxWidth())
                    SheetText(playing, ::openEpisode, ::openShow)
                    Scrubber(playing, big = true)
                    Controls(playing, big = true)
                    SheetExtras(playing, onNotes = ::openEpisode)
                }
            }
        }
    }
}

@Composable
private fun CloseHandle(onClose: () -> Unit, modifier: Modifier = Modifier) {
    PodIconButton(PodIcons.ChevronDown, "Close player", onClick = onClose, modifier = modifier, tint = podColors.ink, size = 48.dp, iconSize = 28.dp)
}

/** The cover as the largest square the space allows, whichever of width or height runs out first. */
@Composable
private fun SheetArt(playing: NowPlaying, modifier: Modifier) {
    BoxWithConstraints(modifier, contentAlignment = Alignment.Center) {
        val side = minOf(maxWidth, maxHeight)
        Artwork(playing.artworkUrl, side, playing.podcastTitle, shadow = true, radius = 16.dp)
    }
}

@Composable
private fun SheetText(playing: NowPlaying, onEpisode: () -> Unit, onShow: () -> Unit) {
    val colors = podColors
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(
            playing.title, style = PodType.detailTitle, color = colors.ink, maxLines = 2, overflow = TextOverflow.Ellipsis,
            modifier = Modifier.clip(RoundedCornerShape(6.dp)).clickable(onClickLabel = "Open episode", onClick = onEpisode),
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                playing.podcastTitle, style = PodType.bodyMedium, color = colors.ink.copy(alpha = 0.72f), maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false).clip(RoundedCornerShape(6.dp)).clickable(enabled = playing.podcastId != null, onClick = onShow),
            )
            SourceMark(playing.source)
        }
    }
}

/** "Playing from download" or "Streaming", as a small glyph after the show. Streaming never starts a download (spec 7.4). */
@Composable
private fun SourceMark(source: PlaySource?) {
    val (icon, label) = when (source ?: return) {
        PlaySource.Download -> PodIcons.Downloaded to "Playing from download"
        PlaySource.Stream -> PodIcons.Stream to "Streaming"
    }
    Row(Modifier.padding(start = 10.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Icon(icon, contentDescription = null, tint = podColors.ink.copy(alpha = 0.6f), modifier = Modifier.size(15.dp))
        Text(label, style = PodType.meta, color = podColors.ink.copy(alpha = 0.6f), maxLines = 1)
    }
}

/** Speed, sleep timer, the played toggle and the notes, as quiet pills under the transport. */
@Composable
private fun SheetExtras(playing: NowPlaying, onNotes: () -> Unit) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally), verticalAlignment = Alignment.CenterVertically) {
        SpeedControl(playing.speed, height = 36.dp)
        SleepControl(playing, height = 36.dp)
        PlayedToggle(playing.episodeId)
        OutlinePill(onClick = onNotes, height = 36.dp, description = "Show notes") {
            Icon(PodIcons.Notes, contentDescription = null, tint = podColors.ink, modifier = Modifier.size(18.dp))
        }
    }
}

// ==============================================================================================
// The player bar (wide layouts)
// ==============================================================================================

/**
 * One bar along the bottom: what's playing (tap for the episode), the transport over the
 * scrubber in the middle, and the speed and sleep timer at the end.
 */
@Composable
fun PlayerBar(modifier: Modifier = Modifier) {
    val player = LocalPlayer.current
    val navigator = LocalNavigator.current
    val now by player.state.collectAsState()
    val playing = now ?: return
    val colors = podColors
    Box(modifier.fillMaxWidth().background(colors.surface)) {
        Ambient(playing.artworkUrl, Modifier.matchParentSize())
        Box(Modifier.fillMaxWidth().height(1.dp).background(colors.line))
        Row(
            Modifier.fillMaxWidth().navigationBarsPadding().heightIn(min = 88.dp).padding(horizontal = 24.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(24.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                Modifier.weight(1f).clip(RoundedCornerShape(10.dp)).clickable(onClickLabel = "Open episode") { navigator.open(Screen.Episode(playing.episodeId)) },
                horizontalArrangement = Arrangement.spacedBy(14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Artwork(playing.artworkUrl, 52.dp, playing.podcastTitle, shadow = true, radius = 8.dp)
                Column(Modifier.weight(1f)) {
                    Text(playing.title, style = PodType.rowTitle, color = colors.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(playing.podcastTitle, style = PodType.meta, color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
            Column(Modifier.weight(1.5f).widthIn(max = 600.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Controls(playing, big = false)
                Scrubber(playing, big = false)
            }
            Row(Modifier.weight(1f), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End), verticalAlignment = Alignment.CenterVertically) {
                SleepControl(playing, height = 32.dp)
                SpeedControl(playing.speed, height = 32.dp)
            }
        }
    }
}

// ==============================================================================================
// Shared controls
// ==============================================================================================

/**
 * The position as a thin track with the accent's fill and an ink thumb that grows while held,
 * with elapsed and remaining time: beside it in the bar, below it in the sheet. While the thumb is
 * held the clocks follow it, and the seek goes out on release, so a drag is one seek and one
 * position write.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Scrubber(playing: NowPlaying, big: Boolean) {
    val player = LocalPlayer.current
    val colors = podColors
    val duration = playing.durationMs?.takeIf { it > 0 }
    var held by remember { mutableStateOf<Long?>(null) }
    val position = held ?: playing.positionMs
    val interaction = remember { MutableInteractionSource() }
    val dragging by interaction.collectIsDraggedAsState()
    val pressed by interaction.collectIsPressedAsState()
    val active = dragging || pressed
    val track = if (big) 6.dp else 4.dp
    val thumb = if (big) 18.dp else if (active) 14.dp else 10.dp
    val slider: @Composable (Modifier) -> Unit = { modifier ->
        Slider(
            value = if (duration == null) 0f else (position.toFloat() / duration).coerceIn(0f, 1f),
            onValueChange = { fraction -> if (duration != null) held = (fraction * duration).toLong() },
            onValueChangeFinished = {
                held?.let(player::seekTo)
                held = null
            },
            enabled = duration != null,
            interactionSource = interaction,
            modifier = modifier.height(if (big) 28.dp else 20.dp).semantics { contentDescription = "Position" },
            thumb = { Box(Modifier.size(thumb).clip(CircleShape).background(colors.ink)) },
            track = { state ->
                val fraction = state.value.coerceIn(0f, 1f)
                Canvas(Modifier.fillMaxWidth().height(track)) {
                    val radius = CornerRadius(size.height / 2)
                    drawRoundRect(colors.ink.copy(alpha = 0.16f), cornerRadius = radius)
                    drawRoundRect(colors.accent, size = Size(size.width * fraction, size.height), cornerRadius = radius)
                }
            },
        )
    }
    val elapsed = clockLabel(position)
    val remaining = duration?.let { remainingLabel(position, it) } ?: "--:--"
    if (big) {
        Column(Modifier.fillMaxWidth()) {
            slider(Modifier.fillMaxWidth())
            Row(Modifier.fillMaxWidth()) {
                Text(elapsed, style = PodType.clock, color = colors.ink.copy(alpha = 0.7f))
                Spacer(Modifier.weight(1f))
                Text(remaining, style = PodType.clock, color = colors.ink.copy(alpha = 0.7f))
            }
        }
    } else {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(elapsed, style = PodType.clock.copy(fontSize = 12.sp), color = colors.muted, textAlign = androidx.compose.ui.text.style.TextAlign.End, modifier = Modifier.width(52.dp))
            slider(Modifier.weight(1f))
            Text(remaining, style = PodType.clock.copy(fontSize = 12.sp), color = colors.muted, modifier = Modifier.width(52.dp))
        }
    }
}

/** Skip back, play/pause (an ink circle), skip forward. */
@Composable
private fun Controls(playing: NowPlaying, big: Boolean) {
    val player = LocalPlayer.current
    val colors = podColors
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = if (big) Arrangement.SpaceEvenly else Arrangement.spacedBy(18.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        SkipButton(playing.skipBackMs, forward = false, big = big, onSkip = player::skipBack) { player.setSkipInterval(false, it) }
        val size = if (big) 76.dp else 44.dp
        Box(
            Modifier
                .size(size)
                .clip(CircleShape)
                .background(colors.ink)
                .clickable(role = Role.Button) { player.toggle() }
                .semantics { contentDescription = if (playing.playing) "Pause" else "Play" },
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                if (playing.playing) PodIcons.Pause else PodIcons.Play, contentDescription = null, tint = colors.background,
                modifier = Modifier.size(if (big) 34.dp else 20.dp).padding(start = if (playing.playing) 0.dp else 2.dp),
            )
        }
        SkipButton(playing.skipForwardMs, forward = true, big = big, onSkip = player::skipForward) { player.setSkipInterval(true, it) }
    }
}

/**
 * A circular arrow with the interval's seconds inside it. A long press picks another interval,
 * which changes the synced setting on every device.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SkipButton(intervalMs: Long, forward: Boolean, big: Boolean, onSkip: () -> Unit, onPick: (Long) -> Unit) {
    val colors = podColors
    var picking by remember { mutableStateOf(false) }
    val label = skipLabel(intervalMs)
    val description = if (forward) "Skip forward $label" else "Skip back $label"
    val size = if (big) 60.dp else 40.dp
    Box {
        Box(
            Modifier
                .size(size)
                .clip(CircleShape)
                .combinedClickable(onClick = onSkip, onLongClick = { picking = true }, onLongClickLabel = "Change interval", role = Role.Button)
                .semantics { contentDescription = description },
            contentAlignment = Alignment.Center,
        ) {
            Icon(if (forward) PodIcons.SkipForward else PodIcons.SkipBack, contentDescription = null, tint = colors.ink, modifier = Modifier.size(if (big) 34.dp else 24.dp))
            Text(
                "${intervalMs / 1_000}",
                style = PodType.clock.copy(fontFamily = RadioCanada, fontSize = if (big) 11.sp else 8.5.sp, fontWeight = FontWeight.Bold, lineHeight = if (big) 11.sp else 9.sp),
                color = colors.ink,
                modifier = Modifier.offset(y = if (big) 1.dp else 0.5.dp),
            )
        }
        DropdownMenu(picking, onDismissRequest = { picking = false }, shape = RoundedCornerShape(12.dp), containerColor = colors.surface) {
            SKIP_STEPS_MS.forEach { step ->
                DropdownMenuItem(
                    text = { Text(skipLabel(step)) },
                    trailingIcon = if (step == intervalMs) ({ Icon(PodIcons.Check, contentDescription = "Current", tint = colors.accentInk, modifier = Modifier.size(18.dp)) }) else null,
                    onClick = {
                        picking = false
                        onPick(step)
                    },
                )
            }
        }
    }
}

/** A pill outlined in ink at 22 %, as the web's speed button, holding whatever [content] is. */
@Composable
private fun OutlinePill(onClick: () -> Unit, height: Dp, description: String? = null, content: @Composable () -> Unit) {
    val colors = podColors
    Row(
        Modifier
            .heightIn(min = height)
            .clip(CircleShape)
            .border(1.dp, colors.ink.copy(alpha = 0.22f), CircleShape)
            .clickable(role = Role.Button, onClick = onClick)
            .then(if (description != null) Modifier.semantics { contentDescription = description } else Modifier)
            .padding(horizontal = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) { content() }
}

/** The synced speed. Changing it here changes it on every device. */
@Composable
private fun SpeedControl(speed: Float, height: Dp) {
    val player = LocalPlayer.current
    val colors = podColors
    var open by remember { mutableStateOf(false) }
    Box {
        OutlinePill(onClick = { open = true }, height = height, description = "Playback speed ${speedLabel(speed)}") {
            Text(speedLabel(speed), style = PodType.label.tabular(), color = colors.ink, modifier = Modifier.widthIn(min = 28.dp), textAlign = androidx.compose.ui.text.style.TextAlign.Center)
        }
        DropdownMenu(open, onDismissRequest = { open = false }, shape = RoundedCornerShape(12.dp), containerColor = colors.surface) {
            SPEEDS.forEach { step ->
                DropdownMenuItem(
                    text = { Text(speedLabel(step.toFloat())) },
                    trailingIcon = if (step.toFloat() == speed) ({ Icon(PodIcons.Check, contentDescription = "Current", tint = colors.accentInk, modifier = Modifier.size(18.dp)) }) else null,
                    onClick = {
                        open = false
                        player.setSpeed(step.toFloat())
                    },
                )
            }
        }
    }
}

/** The sleep timer, a moon with the countdown beside it while set. */
@Composable
private fun SleepControl(playing: NowPlaying, height: Dp) {
    val player = LocalPlayer.current
    val colors = podColors
    var open by remember { mutableStateOf(false) }
    val now = rememberNow(1_000)
    val label = sleepLabel(playing.sleepAt, playing.sleepAtEndOfEpisode, now)
    Box {
        OutlinePill(onClick = { open = true }, height = height, description = "Sleep timer" + (label?.let { ", $it" } ?: "")) {
            Icon(PodIcons.Sleep, contentDescription = null, tint = if (label != null) colors.accentInk else colors.ink, modifier = Modifier.size(18.dp))
            if (label != null) Text(label, style = PodType.label.tabular(), color = colors.accentInk)
        }
        DropdownMenu(open, onDismissRequest = { open = false }, shape = RoundedCornerShape(12.dp), containerColor = colors.surface) {
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
    val colors = podColors
    val episode by remember(episodeId) { pod.database.observe(*EPISODE_TABLES) { loadEpisode(it, episodeId) } }.collectAsState(initial = null)
    val loaded = episode ?: return
    OutlinePill(onClick = { scope.launch { writeLocal(pod.database, pod.sync, played(loaded.id, loaded.podcastId, !loaded.played)) } }, height = 36.dp) {
        Icon(PodIcons.Check, contentDescription = null, tint = if (loaded.played) colors.accentInk else colors.ink, modifier = Modifier.size(18.dp))
        Text(if (loaded.played) "Played" else "Mark played", style = PodType.metaMedium, color = if (loaded.played) colors.accentInk else colors.ink)
    }
}
