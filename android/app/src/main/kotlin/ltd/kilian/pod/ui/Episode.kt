package ltd.kilian.pod.ui

import android.content.Intent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.fromHtml
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import java.time.ZoneId
import kotlinx.coroutines.launch
import ltd.kilian.pod.episode.EPISODE_TABLES
import ltd.kilian.pod.episode.EpisodeDetail
import ltd.kilian.pod.episode.archived
import ltd.kilian.pod.episode.loadEpisode
import ltd.kilian.pod.episode.played
import ltd.kilian.pod.inbox.dayLabel
import ltd.kilian.pod.inbox.durationLabel
import ltd.kilian.pod.inbox.progress
import ltd.kilian.pod.inbox.timeLeftLabel
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.writeLocal

// ==============================================================================================
// Episode (wireframe M7)
// ==============================================================================================
//
// The primary action reads "Play", "Resume · 21 min left" or "Play again". Show notes are
// sanitized HTML (spec 2.2), rendered natively; links open in the browser. The overflow holds
// Share link and Go to podcast.

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EpisodeScreen(id: String) {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val now = rememberNow()
    val zone = remember { ZoneId.systemDefault() }
    val loaded by remember(id) { pod.database.observe(*EPISODE_TABLES) { Optional(loadEpisode(it, id)) } }.collectAsState(initial = null)
    var menu by remember { mutableStateOf(false) }

    val episode = (loaded ?: return).value ?: run {
        // Deleted by the feed (a tombstone), or its show was unsubscribed.
        navigator.back()
        return
    }
    fun write(change: ltd.kilian.pod.sync.LocalChange) = scope.launch { writeLocal(pod.database, pod.sync, change) }

    Column(Modifier.fillMaxSize()) {
        TopAppBar(
            title = {},
            navigationIcon = { IconButton(onClick = { navigator.back() }) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") } },
            actions = {
                IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "More") }
                DropdownMenu(menu, onDismissRequest = { menu = false }) {
                    episode.link?.let { link ->
                        DropdownMenuItem(text = { Text("Share link") }, onClick = {
                            menu = false
                            val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, link)
                            context.startActivity(Intent.createChooser(send, null))
                        })
                    }
                    DropdownMenuItem(text = { Text("Go to podcast") }, onClick = {
                        menu = false
                        navigator.open(Screen.Podcast(episode.podcastId))
                    })
                }
            },
        )
        Column(Modifier.verticalScroll(rememberScrollState()).padding(16.dp)) {
            Row {
                Artwork(episode.artworkUrl, 72.dp)
                Spacer(Modifier.width(12.dp))
                Column {
                    Text(episode.podcastTitle, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
                    Text(episode.title, style = MaterialTheme.typography.titleLarge)
                    Text(
                        listOfNotNull(dayLabel(episode.publishedAt, now, zone), episode.durationMs?.let(::durationLabel)).joinToString(" · "),
                        style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Spacer(Modifier.height(16.dp))
            progress(episode.durationMs, episode.positionMs)?.takeUnless { episode.played }?.let { fraction ->
                LinearProgressIndicator(progress = { fraction }, Modifier.fillMaxWidth())
                Text("${durationLabel(episode.positionMs)} of ${durationLabel(episode.durationMs!!)}", style = MaterialTheme.typography.labelSmall)
                Spacer(Modifier.height(8.dp))
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PlayButton(episode, Modifier.weight(1f))
                DownloadButton(episode.id)
            }
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { write(played(episode.id, episode.podcastId, !episode.played)) }) {
                    Icon(Icons.Outlined.CheckCircle, contentDescription = null)
                    Text(if (episode.played) " Mark unplayed" else " Mark played")
                }
                OutlinedButton(onClick = { write(archived(episode.id, episode.podcastId, !episode.archived)) }) {
                    Icon(if (episode.archived) Icons.Outlined.Unarchive else Icons.Outlined.Archive, contentDescription = null)
                    Text(if (episode.archived) " Unarchive" else " Archive")
                }
            }
            Spacer(Modifier.height(24.dp))
            Text("Show notes", style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            ShowNotes(episode.showNotesHtml)
        }
    }
}

/** Wraps a nullable query result, so "no such episode" and "not read yet" differ. */
private data class Optional<T>(val value: T?)

/** "Play", "Resume · 21 min left" or "Play again" for a played episode (spec 8: replaying writes played = false once). */
@Composable
private fun PlayButton(episode: EpisodeDetail, modifier: Modifier) {
    val player = rememberPlayer()
    val label = when {
        episode.played -> "Play again"
        else -> timeLeftLabel(episode.durationMs, episode.positionMs)?.let { "Resume · $it" } ?: "Play"
    }
    Button(onClick = { player.play(episode.id) }, modifier = modifier) {
        Icon(Icons.Outlined.PlayArrow, contentDescription = null)
        Text(" $label")
    }
}

/** Sanitized show notes as styled text. Links are underlined and open in the browser. */
@Composable
private fun ShowNotes(html: String) {
    val linkColor = MaterialTheme.colorScheme.primary
    val text = remember(html, linkColor) {
        AnnotatedString.fromHtml(
            html,
            linkStyles = TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)),
        )
    }
    Text(text, style = MaterialTheme.typography.bodyMedium)
}
