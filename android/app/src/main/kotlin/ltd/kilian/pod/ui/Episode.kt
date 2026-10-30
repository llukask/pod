package ltd.kilian.pod.ui

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.ZoneId
import kotlinx.coroutines.launch
import ltd.kilian.pod.episode.EPISODE_TABLES
import ltd.kilian.pod.episode.EpisodeDetail
import ltd.kilian.pod.episode.archived
import ltd.kilian.pod.episode.loadEpisode
import ltd.kilian.pod.episode.played
import ltd.kilian.pod.inbox.dayLabel
import ltd.kilian.pod.inbox.durationLabel
import ltd.kilian.pod.inbox.timeLeftLabel
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.writeLocal

// ==============================================================================================
// Episode (wireframe M7)
// ==============================================================================================
//
// What the episode is, what you can do with it, and its notes. The primary action reads "Play",
// "Pause" while it plays, "Resume · 21 min left" once started, or "Play again" once played. Show
// notes are sanitized HTML (spec 2.2), rendered natively; links open in the browser. The overflow
// holds Share link and Go to podcast.
//
// On a phone this is a page of its own with a back affordance naming where it goes. Beside a list
// (wide layouts, [inPane]) it has a close button instead, and its header sits in one row.

@Composable
fun EpisodeScreen(id: String, inPane: Boolean = false) {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val now = rememberNow()
    val zone = remember { ZoneId.systemDefault() }
    val colors = podColors
    val loaded by remember(id) { pod.database.observe(*EPISODE_TABLES) { Optional(loadEpisode(it, id)) } }.collectAsState(initial = null)
    var menu by remember { mutableStateOf(false) }

    val episode = (loaded ?: return).value ?: run {
        // Deleted by the feed (a tombstone), or its show was unsubscribed.
        navigator.back()
        return
    }
    fun write(change: ltd.kilian.pod.sync.LocalChange) = scope.launch { writeLocal(pod.database, pod.sync, change) }
    fun openShow() = navigator.open(Screen.Podcast(episode.podcastId))

    val backLabel = when (val previous = navigator.previous) {
        is Screen.Tab -> previous.label
        is Screen.Podcast -> episode.podcastTitle
        Screen.Search -> "Search"
        is Screen.Episode, null -> null
    }
    Column(Modifier.fillMaxSize()) {
        val overflow: @Composable () -> Unit = {
            Box {
                PodIconButton(PodIcons.More, "More", onClick = { menu = true })
                DropdownMenu(menu, onDismissRequest = { menu = false }, shape = RoundedCornerShape(12.dp), containerColor = colors.surface) {
                    episode.link?.let { link ->
                        DropdownMenuItem(text = { Text("Share link") }, onClick = {
                            menu = false
                            val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, link)
                            context.startActivity(Intent.createChooser(send, null))
                        })
                    }
                    DropdownMenuItem(text = { Text("Go to podcast") }, onClick = {
                        menu = false
                        openShow()
                    })
                }
            }
        }
        if (inPane) {
            Row(Modifier.fillMaxWidth().padding(top = 8.dp, end = 10.dp), horizontalArrangement = Arrangement.End) {
                overflow()
                PodIconButton(PodIcons.Close, "Close", onClick = { navigator.back() })
            }
        } else {
            BackBar(backLabel, onBack = { navigator.back() }) { overflow() }
        }
        BoxWithConstraints(Modifier.fillMaxSize()) {
            val wide = maxWidth >= 560.dp
            val gutter = if (wide) 28.dp else LocalGutter.current
            Column(Modifier.verticalScroll(rememberScrollState()).padding(horizontal = gutter).padding(top = if (inPane) 0.dp else 4.dp, bottom = 40.dp).widthIn(max = 760.dp)) {
                Header(episode, now, zone, wide, onShow = ::openShow)
                Spacer(Modifier.height(if (wide) 24.dp else 20.dp))
                Actions(episode, wide, onPlayed = { write(played(episode.id, episode.podcastId, !episode.played)) }, onArchived = { write(archived(episode.id, episode.podcastId, !episode.archived)) }) {
                    episode.link?.let { link -> context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(link))) }
                }
                Spacer(Modifier.height(if (wide) 28.dp else 24.dp))
                Box(Modifier.fillMaxWidth().height(1.dp).background(colors.line))
                Spacer(Modifier.height(24.dp))
                ShowNotes(episode.showNotesHtml)
            }
        }
    }
}

@Composable
private fun Header(episode: EpisodeDetail, now: Long, zone: ZoneId, wide: Boolean, onShow: () -> Unit) {
    val colors = podColors
    val text: @Composable () -> Unit = {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                episode.podcastTitle, style = PodType.label.copy(fontWeight = FontWeight(600)), color = colors.accentInk,
                modifier = Modifier.clip(RoundedCornerShape(4.dp)).clickable(onClick = onShow),
            )
            Text(episode.title, style = if (wide) PodType.detailTitle else PodType.detailTitle.copy(fontSize = 24.sp, lineHeight = 29.sp), color = colors.ink)
            Text(
                listOfNotNull(dayLabel(episode.publishedAt, now, zone), episode.durationMs?.let(::durationLabel), if (episode.played) "Played" else null).joinToString(" · "),
                style = PodType.meta.tabular(), color = colors.muted,
            )
        }
    }
    if (wide) {
        Row(horizontalArrangement = Arrangement.spacedBy(20.dp), verticalAlignment = Alignment.Bottom) {
            Artwork(episode.artworkUrl, 112.dp, episode.podcastTitle, shadow = true)
            Box(Modifier.weight(1f)) { text() }
        }
    } else {
        Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Artwork(episode.artworkUrl, 132.dp, episode.podcastTitle, shadow = true)
            text()
        }
    }
}

/** Play first and widest; Mark played and Archive as a pair; then the download and the episode's web page. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Actions(episode: EpisodeDetail, wide: Boolean, onPlayed: () -> Unit, onArchived: () -> Unit, onLink: () -> Unit) {
    val player = LocalPlayer.current
    val now by player.state.collectAsState()
    val current = now?.episodeId == episode.id
    val playing = current && now!!.playing
    val position = if (current) now!!.positionMs else episode.positionMs
    val label = when {
        playing -> "Pause"
        episode.played -> "Play again"
        else -> timeLeftLabel(episode.durationMs, position)?.let { "Resume · $it" } ?: "Play"
    }
    val play = { if (current) player.toggle() else player.play(episode.id) }
    val playIcon = if (playing) PodIcons.Pause else PodIcons.Play
    val playedLabel = if (episode.played) "Mark unplayed" else "Mark played"
    val archiveLabel = if (episode.archived) "Return to inbox" else "Archive"
    if (wide) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            PodButton(label, play, kind = ButtonKind.Primary, icon = playIcon, style = PodType.button.tabular())
            PodButton(playedLabel, onPlayed, icon = if (episode.played) PodIcons.Undo else PodIcons.Check)
            PodButton(archiveLabel, onArchived, icon = if (episode.archived) PodIcons.Unarchive else PodIcons.Archive)
            DownloadButton(episode.id, episode.podcastId)
            if (episode.link != null) PodButton("Episode page", onLink, kind = ButtonKind.Quiet, icon = PodIcons.External)
        }
    } else {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            PodButton(label, play, Modifier.fillMaxWidth(), kind = ButtonKind.Primary, icon = playIcon, height = 48.dp, style = PodType.button.copy(fontSize = 16.sp).tabular())
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PodButton(playedLabel, onPlayed, Modifier.weight(1f), icon = if (episode.played) PodIcons.Undo else PodIcons.Check, height = 44.dp)
                PodButton(archiveLabel, onArchived, Modifier.weight(1f), icon = if (episode.archived) PodIcons.Unarchive else PodIcons.Archive, height = 44.dp)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                DownloadButton(episode.id, episode.podcastId, Modifier.weight(1f), height = 44.dp)
                if (episode.link != null) PodButton("Episode page", onLink, Modifier.weight(1f), kind = ButtonKind.Quiet, icon = PodIcons.External, height = 44.dp)
            }
        }
    }
}

/** Wraps a nullable query result, so "no such episode" and "not read yet" differ. */
private data class Optional<T>(val value: T?)
