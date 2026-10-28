package ltd.kilian.pod.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
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
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.time.ZoneId
import kotlinx.coroutines.launch
import ltd.kilian.pod.episode.archived
import ltd.kilian.pod.episode.played
import ltd.kilian.pod.inbox.INBOX_TABLES
import ltd.kilian.pod.inbox.InboxEpisode
import ltd.kilian.pod.inbox.Loading
import ltd.kilian.pod.inbox.dayLabel
import ltd.kilian.pod.inbox.durationLabel
import ltd.kilian.pod.inbox.loadInbox
import ltd.kilian.pod.inbox.loadLoading
import ltd.kilian.pod.inbox.progress
import ltd.kilian.pod.inbox.timeLeftLabel
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.writeLocal

// ==============================================================================================
// Inbox (wireframe M2)
// ==============================================================================================
//
// The landing screen. Swipe right marks played, swipe left archives; both are local writes that
// show at once (spec 5.2), with an undo snackbar. Pull to refresh runs a sync.

/** The inbox list, or null before the first read. */
private data class InboxData(val episodes: List<InboxEpisode>, val loading: Loading?)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun InboxScreen() {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val scope = rememberCoroutineScope()
    val now = rememberNow()
    // The window moves with the clock, so the query re-runs on the tick as well as on writes.
    val data by remember(now / 60_000) {
        pod.database.observe(*INBOX_TABLES, "sync_scope") { InboxData(loadInbox(it, now), loadLoading(it)) }
    }.collectAsState(initial = null)
    var refreshing by remember { mutableStateOf(false) }
    val zone = remember { ZoneId.systemDefault() }

    Column(Modifier.fillMaxSize()) {
        TopAppBar(title = { Text("Inbox") }, actions = { SyncChip() })
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                refreshing = true
                scope.launch {
                    pod.engine.sync()
                    refreshing = false
                }
            },
            modifier = Modifier.fillMaxSize(),
        ) {
            val inbox = data ?: return@PullToRefreshBox
            if (inbox.episodes.isEmpty()) {
                EmptyInbox(inbox.loading, onPodcasts = { navigator.tab(Screen.Tab.Podcasts) })
                return@PullToRefreshBox
            }
            LazyColumn(Modifier.fillMaxSize()) {
                items(inbox.episodes, key = { it.id }) { episode ->
                    SwipeableRow(
                        onPlayed = {
                            scope.launch {
                                writeLocal(pod.database, pod.sync, played(episode.id, episode.podcastId, true))
                                navigator.tell("Marked as played") { writeLocal(pod.database, pod.sync, played(episode.id, episode.podcastId, false)) }
                            }
                        },
                        onArchived = {
                            scope.launch {
                                writeLocal(pod.database, pod.sync, archived(episode.id, episode.podcastId, true))
                                navigator.tell("Archived") { writeLocal(pod.database, pod.sync, archived(episode.id, episode.podcastId, false)) }
                            }
                        },
                    ) {
                        InboxRow(episode, dayLabel(episode.publishedAt, now, zone), onClick = { navigator.open(Screen.Episode(episode.id)) })
                    }
                    HorizontalDivider()
                }
            }
        }
    }
}

@Composable
private fun EmptyInbox(loading: Loading?, onPodcasts: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        if (loading != null && loading.showsTotal > 0) {
            Text("Loading your podcasts…", style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            Text("${loading.showsDone} of ${loading.showsTotal} shows · ${loading.episodes} episodes so far")
            Spacer(Modifier.height(16.dp))
            LinearProgressIndicator(progress = { loading.showsDone.toFloat() / loading.showsTotal }, Modifier.width(200.dp))
        } else {
            Text("You're all caught up", style = MaterialTheme.typography.titleMedium)
            TextButton(onClick = onPodcasts) { Text("Browse your podcasts") }
        }
    }
}

/** Swipe right to mark played, left to archive. The row snaps back; the list drops it once the write lands. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SwipeableRow(onPlayed: () -> Unit, onArchived: () -> Unit, content: @Composable () -> Unit) {
    val state = rememberSwipeToDismissBoxState(
        confirmValueChange = { value ->
            when (value) {
                SwipeToDismissBoxValue.StartToEnd -> onPlayed()
                SwipeToDismissBoxValue.EndToStart -> onArchived()
                SwipeToDismissBoxValue.Settled -> Unit
            }
            // Never stay dismissed: the live query removes the row, and an undo brings it back
            // in its old place.
            false
        },
    )
    SwipeToDismissBox(
        state = state,
        backgroundContent = {
            val toPlayed = state.dismissDirection == SwipeToDismissBoxValue.StartToEnd
            Row(
                Modifier.fillMaxSize().background(
                    if (toPlayed) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.tertiaryContainer,
                ).padding(horizontal = 24.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = if (toPlayed) Arrangement.Start else Arrangement.End,
            ) {
                Icon(if (toPlayed) Icons.Outlined.CheckCircle else Icons.Outlined.Archive, contentDescription = null)
                Spacer(Modifier.width(8.dp))
                Text(if (toPlayed) "Played" else "Archive")
            }
        },
    ) {
        Box(Modifier.background(MaterialTheme.colorScheme.surface)) { content() }
    }
}

@Composable
private fun InboxRow(episode: InboxEpisode, day: String, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 16.dp, vertical = 12.dp)) {
        Artwork(episode.artworkUrl, 56.dp)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(
                listOfNotNull(episode.podcastTitle, day, episode.durationMs?.let(::durationLabel)).joinToString(" · "),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            Text(episode.title, style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            timeLeftLabel(episode.durationMs, episode.positionMs)?.let { left ->
                Spacer(Modifier.height(4.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    LinearProgressIndicator(progress = { progress(episode.durationMs, episode.positionMs) ?: 0f }, Modifier.width(64.dp))
                    Spacer(Modifier.width(8.dp))
                    Text(left, style = MaterialTheme.typography.labelSmall)
                }
            }
        }
        DownloadGlyph(episode.id)
    }
}
