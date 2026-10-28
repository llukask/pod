package ltd.kilian.pod.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Remove
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.fromHtml
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.time.ZoneId
import kotlinx.coroutines.launch
import ltd.kilian.pod.inbox.dayLabel
import ltd.kilian.pod.inbox.durationLabel
import ltd.kilian.pod.podcast.AUTO_KEEP_MAX
import ltd.kilian.pod.podcast.DeviceSettings
import ltd.kilian.pod.podcast.PODCAST_TABLES
import ltd.kilian.pod.podcast.PodcastEpisode
import ltd.kilian.pod.podcast.PodcastPage
import ltd.kilian.pod.podcast.PodcastState
import ltd.kilian.pod.podcast.autoLabel
import ltd.kilian.pod.podcast.loadPage
import ltd.kilian.pod.podcast.markAllPlayed
import ltd.kilian.pod.podcast.markAllPlayedTitle
import ltd.kilian.pod.podcast.progressLabel
import ltd.kilian.pod.podcast.setDeviceSettings
import ltd.kilian.pod.podcast.statusNotice
import ltd.kilian.pod.podcast.unsubscribe
import ltd.kilian.pod.podcast.unsubscribeMessage
import ltd.kilian.pod.store.observe

// ==============================================================================================
// Podcast (wireframes M5 and M6)
// ==============================================================================================
//
// The show's whole history from the local mirror, newest first, with filter chips. The gear
// opens this device's settings for the show; the overflow holds "Mark all played" and
// Unsubscribe, which confirms with what goes (M11).

private enum class Filter(val label: String) { All("All"), Unplayed("Unplayed"), Downloaded("Downloaded") }

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PodcastScreen(id: String) {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val scope = rememberCoroutineScope()
    val now = rememberNow()
    val zone = remember { ZoneId.systemDefault() }
    // A page that was there and then isn't means the show left (unsubscribed here or elsewhere,
    // or replaced by its canonical id); the screen goes back rather than showing nothing.
    val page by remember(id) { pod.database.observe(*PODCAST_TABLES) { Loaded(loadPage(it, id)) } }.collectAsState(initial = null)
    var filter by rememberSaveable { mutableStateOf(Filter.All) }
    var settingsSheet by rememberSaveable { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var confirmUnsubscribe by rememberSaveable { mutableStateOf(false) }
    var confirmMarkAll by rememberSaveable { mutableStateOf(false) }

    val loaded = page ?: return
    val show = loaded.page ?: run {
        navigator.back()
        return
    }
    val downloaded = rememberDownloadedEpisodes(id)
    val episodes = when (filter) {
        Filter.All -> show.episodes
        Filter.Unplayed -> show.episodes.filterNot { it.played }
        Filter.Downloaded -> show.episodes.filter { it.id in downloaded }
    }
    val unplayed = show.episodes.count { !it.played }

    Column(Modifier.fillMaxSize()) {
        TopAppBar(
            title = {},
            navigationIcon = { IconButton(onClick = { navigator.back() }) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") } },
            actions = {
                IconButton(onClick = { settingsSheet = true }) { Icon(Icons.Outlined.Settings, "Podcast settings") }
                IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, "More") }
                DropdownMenu(menu, onDismissRequest = { menu = false }) {
                    DropdownMenuItem(text = { Text("Mark all played") }, enabled = unplayed > 0, onClick = {
                        menu = false
                        confirmMarkAll = true
                    })
                    DropdownMenuItem(text = { Text("Unsubscribe") }, onClick = {
                        menu = false
                        confirmUnsubscribe = true
                    })
                }
            },
        )
        LazyColumn(Modifier.fillMaxSize()) {
            item { Header(show, now, onSettings = { settingsSheet = true }) }
            item {
                Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Filter.entries.forEach { FilterChip(filter == it, onClick = { filter = it }, label = { Text(it.label) }) }
                }
            }
            if (show.loading) {
                item {
                    Column(Modifier.padding(16.dp)) {
                        Text("Loading episodes… ${show.episodes.size} so far", style = MaterialTheme.typography.bodySmall)
                        LinearProgressIndicator(Modifier.fillMaxWidth())
                    }
                }
            }
            items(episodes, key = { it.id }) { episode ->
                EpisodeListRow(episode, dayLabel(episode.publishedAt, now, zone)) { navigator.open(Screen.Episode(episode.id)) }
                HorizontalDivider()
            }
        }
    }

    if (settingsSheet) {
        PodcastSettingsSheet(show, onDismiss = { settingsSheet = false }, onUnsubscribe = {
            settingsSheet = false
            confirmUnsubscribe = true
        })
    }
    if (confirmUnsubscribe) {
        AlertDialog(
            onDismissRequest = { confirmUnsubscribe = false },
            title = { Text("Unsubscribe from ${show.title}?") },
            text = { Text(unsubscribeMessage(show.downloads)) },
            dismissButton = { TextButton(onClick = { confirmUnsubscribe = false }) { Text("Cancel") } },
            confirmButton = {
                TextButton(onClick = {
                    confirmUnsubscribe = false
                    scope.launch { unsubscribe(pod.database, pod.sync, show.id, show.feedUrl) }
                }) { Text("Unsubscribe") }
            },
        )
    }
    if (confirmMarkAll) {
        AlertDialog(
            onDismissRequest = { confirmMarkAll = false },
            title = { Text(markAllPlayedTitle(unplayed)) },
            text = { Text("They leave the inbox, and their downloads are removed from this phone.") },
            dismissButton = { TextButton(onClick = { confirmMarkAll = false }) { Text("Cancel") } },
            confirmButton = {
                TextButton(onClick = {
                    confirmMarkAll = false
                    scope.launch { markAllPlayed(pod.database, pod.sync, show.id) }
                }) { Text("Mark played") }
            },
        )
    }
}

/** The page query's result, so "not subscribed" and "not read yet" differ. */
private data class Loaded(val page: PodcastPage?)

@Composable
private fun Header(show: PodcastPage, now: Long, onSettings: () -> Unit) {
    Column(Modifier.padding(16.dp)) {
        Row {
            Artwork(show.artworkUrl, 112.dp)
            Spacer(Modifier.width(16.dp))
            Column {
                Text(show.title, style = MaterialTheme.typography.titleLarge)
                show.author?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                Spacer(Modifier.height(8.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(Icons.Outlined.Check, contentDescription = null)
                    Text(" Subscribed", style = MaterialTheme.typography.labelLarge)
                }
                autoLabel(show.device)?.let { TextButton(onClick = onSettings) { Text(it) } }
            }
        }
        if (show.state == PodcastState.Pending) {
            Spacer(Modifier.height(12.dp))
            Text("Waiting for the server to fetch this feed.", style = MaterialTheme.typography.bodyMedium)
        }
        statusNotice(show.state, show.episodes.size, show.downloads.total, now)?.let { notice ->
            Spacer(Modifier.height(12.dp))
            Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer)) {
                Text(notice, Modifier.padding(12.dp), color = MaterialTheme.colorScheme.onErrorContainer)
            }
        }
        show.description?.let {
            Spacer(Modifier.height(12.dp))
            // Feeds put HTML in the description too; the header shows its text.
            Text(remember(it) { AnnotatedString.fromHtml(it) }, style = MaterialTheme.typography.bodyMedium, maxLines = 3, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** A row of the podcast's list. Played episodes are dimmed with a check; archived ones carry a label but stay. */
@Composable
fun EpisodeListRow(episode: PodcastEpisode, day: String, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 16.dp, vertical = 12.dp).alpha(if (episode.played) 0.55f else 1f),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                listOfNotNull(day, episode.durationMs?.let(::durationLabel), if (episode.archived) "Archived" else null).joinToString(" · "),
                style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(episode.title, style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            progressLabel(episode)?.let { Text(it, style = MaterialTheme.typography.labelSmall) }
        }
        DownloadGlyph(episode.id)
    }
}

/**
 * This device's settings for the show (wireframe M6), a bottom sheet. Everything here is
 * device-only (`podcast_device`, spec 7.1) and says so.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PodcastSettingsSheet(show: PodcastPage, onDismiss: () -> Unit, onUnsubscribe: () -> Unit) {
    val pod = LocalPod.current
    val scope = rememberCoroutineScope()
    fun save(settings: DeviceSettings) = scope.launch { setDeviceSettings(pod.database, show.id, settings) }
    val device = show.device
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(bottom = 24.dp)) {
            ListItem(headlineContent = { Text(show.title, style = MaterialTheme.typography.titleLarge) }, supportingContent = { Text("This device only") })
            ListItem(
                headlineContent = { Text("New episode notifications") },
                supportingContent = { Text("Only for episodes from the last 48 hours") },
                trailingContent = { Switch(device.notify, onCheckedChange = { save(device.copy(notify = it)) }) },
            )
            ListItem(
                headlineContent = { Text("Auto-download") },
                supportingContent = { Text(if (device.autoKeep == 0) "Off" else "Keep the newest ${device.autoKeep} unplayed episodes") },
                trailingContent = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        IconButton(onClick = { save(device.copy(autoKeep = device.autoKeep - 1)) }, enabled = device.autoKeep > 0) {
                            Icon(Icons.Outlined.Remove, "Fewer")
                        }
                        Text("${device.autoKeep}")
                        IconButton(onClick = { save(device.copy(autoKeep = device.autoKeep + 1)) }, enabled = device.autoKeep < AUTO_KEEP_MAX) {
                            Icon(Icons.Outlined.Add, "More")
                        }
                    }
                },
            )
            ListItem(
                headlineContent = { Text("Auto-download on mobile data") },
                supportingContent = { Text("Otherwise waits for Wi-Fi") },
                trailingContent = { Switch(device.autoOnMetered, onCheckedChange = { save(device.copy(autoOnMetered = it)) }, enabled = device.autoKeep > 0) },
            )
            HorizontalDivider()
            ListItem(
                headlineContent = { Text("Unsubscribe", color = MaterialTheme.colorScheme.error) },
                modifier = Modifier.clickable(onClick = onUnsubscribe),
            )
        }
    }
}
