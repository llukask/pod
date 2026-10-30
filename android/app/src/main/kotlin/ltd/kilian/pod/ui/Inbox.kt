package ltd.kilian.pod.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
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
import androidx.compose.ui.unit.dp
import java.time.Instant
import java.time.ZoneId
import java.time.temporal.ChronoUnit
import kotlinx.coroutines.launch
import ltd.kilian.pod.episode.archived
import ltd.kilian.pod.episode.played
import ltd.kilian.pod.inbox.INBOX_TABLES
import ltd.kilian.pod.inbox.InboxEpisode
import ltd.kilian.pod.inbox.Loading
import ltd.kilian.pod.inbox.dayLabel
import ltd.kilian.pod.inbox.loadInbox
import ltd.kilian.pod.inbox.loadLoading
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.writeLocal

// ==============================================================================================
// Inbox (wireframe M2)
// ==============================================================================================
//
// The landing screen, grouped by day: "Today", "Yesterday", "This week", "Earlier", with the
// headings pinned while their rows scroll. Swipe right marks played, swipe left archives; both
// are local writes that show at once (spec 5.2), with an undo snackbar. Pull to refresh runs a
// sync.

/** The inbox list, or null before the first read. */
private data class InboxData(val episodes: List<InboxEpisode>, val loading: Loading?)

/** The day group an episode published at [published] falls in, by calendar days in [zone]. */
fun dayGroup(published: Long, now: Long, zone: ZoneId): String {
    val day = Instant.ofEpochMilli(published).atZone(zone).toLocalDate()
    val today = Instant.ofEpochMilli(now).atZone(zone).toLocalDate()
    return when (ChronoUnit.DAYS.between(day, today)) {
        // A clock running behind the publisher's puts a few episodes in the future; they are new.
        in Long.MIN_VALUE..0L -> "Today"
        1L -> "Yesterday"
        in 2L..6L -> "This week"
        else -> "Earlier"
    }
}

/** The list cut into runs of one day group each; it is newest first, so each group is one run. */
fun <T> groupByDay(items: List<T>, group: (T) -> String): List<Pair<String, List<T>>> {
    val runs = mutableListOf<Pair<String, MutableList<T>>>()
    for (item in items) {
        val label = group(item)
        if (runs.lastOrNull()?.first == label) runs.last().second += item else runs += label to mutableListOf(item)
    }
    return runs
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
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
        PageHeader("Inbox") { SyncChip() }
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
                EmptyInbox(inbox.loading, onAdd = { navigator.open(Screen.Search) })
                return@PullToRefreshBox
            }
            val groups = groupByDay(inbox.episodes) { dayGroup(it.publishedAt, now, zone) }
            BoxWithConstraints(Modifier.fillMaxSize()) {
                val columns = maxWidth >= 720.dp
                LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
                    for ((label, episodes) in groups) {
                        stickyHeader(key = "group:$label") { GroupHeading(label) }
                        itemsIndexed(episodes, key = { _, it -> it.id }) { index, episode ->
                            if (index > 0) RowDivider()
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
                                EpisodeRow(
                                    RowEpisode(episode.id, episode.podcastId, episode.podcastTitle, episode.title, episode.artworkUrl, episode.durationMs, episode.positionMs, played = false, archived = false),
                                    // Under "Today" and "Yesterday" the heading already says when.
                                    day = if (label == "Today" || label == "Yesterday") null else dayLabel(episode.publishedAt, now, zone),
                                    showPodcast = true,
                                    columns = columns,
                                    onClick = { navigator.open(Screen.Episode(episode.id)) },
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun EmptyInbox(loading: Loading?, onAdd: () -> Unit) {
    val colors = podColors
    if (loading != null && loading.showsTotal > 0) {
        Column(
            Modifier.fillMaxSize().padding(32.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterVertically),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("Loading your podcasts…", style = PodType.detailTitle, color = colors.ink)
            Text("${loading.showsDone} of ${loading.showsTotal} shows · ${loading.episodes} episodes so far", style = PodType.body, color = colors.muted)
            Spacer(Modifier.height(8.dp))
            ProgressBar(progress = { loading.showsDone.toFloat() / loading.showsTotal }, Modifier.width(200.dp))
        }
    } else {
        // Scrollable, so pull to refresh still works on an empty inbox.
        LazyColumn(Modifier.fillMaxSize()) {
            item {
                EmptyState(PodIcons.Inbox, "You're all caught up", "New episodes from your podcasts land here.") {
                    PodButton("Add a podcast", onAdd, icon = PodIcons.Plus)
                }
            }
        }
    }
}

/** Swipe right to mark played, left to archive. The row snaps back; the list drops it once the write lands. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SwipeableRow(onPlayed: () -> Unit, onArchived: () -> Unit, content: @Composable () -> Unit) {
    val colors = podColors
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
                Modifier.fillMaxSize().background(if (toPlayed) colors.accentSoft else colors.sidebar).padding(horizontal = 24.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = if (toPlayed) Arrangement.Start else Arrangement.End,
            ) {
                val tint = if (toPlayed) colors.accentInk else colors.ink
                Icon(if (toPlayed) PodIcons.Check else PodIcons.Archive, contentDescription = null, tint = tint, modifier = Modifier.size(20.dp))
                Spacer(Modifier.width(8.dp))
                Text(if (toPlayed) "Played" else "Archive", style = PodType.bodyMedium, color = tint)
            }
        },
    ) {
        Box(Modifier.background(colors.background)) { content() }
    }
}
