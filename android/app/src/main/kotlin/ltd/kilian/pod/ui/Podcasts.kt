package ltd.kilian.pod.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Block
import androidx.compose.material.icons.outlined.NotificationsActive
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.WarningAmber
import androidx.compose.material3.Badge
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import ltd.kilian.pod.core.PodcastStatus
import ltd.kilian.pod.inbox.INBOX_TABLES
import ltd.kilian.pod.inbox.ShowTile
import ltd.kilian.pod.inbox.loadPodcasts
import ltd.kilian.pod.store.observe

// ==============================================================================================
// Podcasts (wireframe M3)
// ==============================================================================================
//
// Subscribed shows, newest episode first, with each one's inbox count. A show subscribed offline
// is a placeholder that is usable at once; failing and gone shows carry a warning.

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PodcastsScreen() {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val now = rememberNow()
    val shows by remember(now / 60_000) { pod.database.observe(*INBOX_TABLES) { loadPodcasts(it, now) } }.collectAsState(initial = null)

    Column(Modifier.fillMaxSize()) {
        TopAppBar(
            title = { Text("Podcasts") },
            actions = {
                IconButton(onClick = { navigator.open(Screen.Search) }) { Icon(Icons.Outlined.Search, contentDescription = "Search") }
                IconButton(onClick = { navigator.open(Screen.Search) }) { Icon(Icons.Outlined.Add, contentDescription = "Add a podcast") }
            },
        )
        val list = shows ?: return@Column
        if (list.isEmpty()) {
            Column(Modifier.fillMaxSize().padding(32.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
                Text("No podcasts yet", style = MaterialTheme.typography.titleMedium)
                TextButton(onClick = { navigator.open(Screen.Search) }) { Text("Find a show") }
            }
            return@Column
        }
        LazyVerticalGrid(
            GridCells.Adaptive(150.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            items(list, key = { it.id }) { show -> ShowCard(show) { navigator.open(Screen.Podcast(show.id)) } }
        }
    }
}

@Composable
private fun ShowCard(show: ShowTile, onClick: () -> Unit) {
    Column(Modifier.clickable(onClick = onClick)) {
        Box {
            Artwork(show.artworkUrl, 150.dp, Modifier.fillMaxWidth().aspectRatio(1f))
            if (show.unplayed > 0) Badge(Modifier.align(Alignment.TopEnd).padding(6.dp)) { Text("${show.unplayed}") }
        }
        Spacer(Modifier.height(6.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            when (show.status) {
                PodcastStatus.Failing -> Icon(Icons.Outlined.WarningAmber, "Feed unreachable", Modifier.size(16.dp), tint = MaterialTheme.colorScheme.error)
                PodcastStatus.Gone -> Icon(Icons.Outlined.Block, "Feed removed", Modifier.size(16.dp), tint = MaterialTheme.colorScheme.error)
                else -> Unit
            }
            Text(show.title, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            if (show.notify) Icon(Icons.Outlined.NotificationsActive, "Notifications on", Modifier.size(16.dp))
        }
        val subtitle = when (show.status) {
            PodcastStatus.Pending -> "Waiting for first sync"
            PodcastStatus.Failing -> "Feed unreachable"
            PodcastStatus.Gone -> "Feed removed · episodes kept"
            PodcastStatus.Ok -> null
        }
        subtitle?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
}
