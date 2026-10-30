package ltd.kilian.pod.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
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
// Subscribed shows as a grid of covers, newest episode first, each with its inbox count, ending
// in a tile that adds another. A show subscribed offline is a placeholder that is usable at once;
// failing and gone shows carry a warning.

@Composable
fun PodcastsScreen() {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val now = rememberNow()
    val shows by remember(now / 60_000) { pod.database.observe(*INBOX_TABLES) { loadPodcasts(it, now) } }.collectAsState(initial = null)
    val gutter = LocalGutter.current

    Column(Modifier.fillMaxSize()) {
        PageHeader("Podcasts") {
            PodIconButton(PodIcons.Search, "Search", onClick = { navigator.open(Screen.Search) }, tint = podColors.ink)
        }
        val list = shows ?: return@Column
        if (list.isEmpty()) {
            EmptyState(PodIcons.Shows, "No podcasts yet", "Search the directory or paste a feed's address.") {
                PodButton("Add a podcast", { navigator.open(Screen.Search) }, icon = PodIcons.Plus)
            }
            return@Column
        }
        LazyVerticalGrid(
            // Two columns on a phone, more as the width allows; the grid isn't capped.
            GridCells.Adaptive(156.dp),
            contentPadding = PaddingValues(start = gutter, end = gutter, top = 8.dp, bottom = 28.dp),
            horizontalArrangement = Arrangement.spacedBy(if (gutter > 16.dp) 20.dp else 14.dp),
            verticalArrangement = Arrangement.spacedBy(22.dp),
        ) {
            items(list, key = { it.id }) { show -> ShowTile(show) { navigator.open(Screen.Podcast(show.id)) } }
            item(key = "add") { AddTile { navigator.open(Screen.Search) } }
        }
    }
}

@Composable
private fun ShowTile(show: ShowTile, onClick: () -> Unit) {
    val colors = podColors
    Column(Modifier.clip(RoundedCornerShape(12.dp)).clickable(onClick = onClick)) {
        Artwork(show.artworkUrl, 168.dp, show.title, Modifier.fillMaxWidth().aspectRatio(1f), shadow = true, radius = 10.dp)
        Spacer(Modifier.height(10.dp))
        Text(show.title, style = PodType.rowTitle, color = colors.ink, maxLines = 2, overflow = TextOverflow.Ellipsis)
        val meta = when (show.status) {
            PodcastStatus.Pending -> "Fetching the feed…"
            PodcastStatus.Failing -> "Feed unreachable"
            PodcastStatus.Gone -> "Feed removed · episodes kept"
            PodcastStatus.Ok -> if (show.unplayed > 0) "${show.unplayed} in your inbox" else "Nothing new"
        }
        Row(Modifier.padding(top = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            when (show.status) {
                PodcastStatus.Failing -> Icon(PodIcons.Warning, "Feed unreachable", Modifier.size(14.dp).padding(end = 0.dp), tint = colors.danger)
                PodcastStatus.Gone -> Icon(PodIcons.Gone, "Feed removed", Modifier.size(14.dp), tint = colors.danger)
                else -> Unit
            }
            if (show.status == PodcastStatus.Failing || show.status == PodcastStatus.Gone) Spacer(Modifier.size(4.dp))
            Text(meta, style = PodType.meta, color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
            if (show.notify) Icon(PodIcons.Bell, "Notifications on", Modifier.padding(start = 6.dp).size(14.dp), tint = colors.muted)
        }
    }
}

/** The grid's last tile: a dashed square with a plus, opening search. */
@Composable
private fun AddTile(onClick: () -> Unit) {
    val colors = podColors
    Column(Modifier.clip(RoundedCornerShape(12.dp)).clickable(onClick = onClick)) {
        Box(
            Modifier
                .fillMaxWidth()
                .aspectRatio(1f)
                .drawBehind {
                    val stroke = 1.5.dp.toPx()
                    drawRoundRect(
                        colors.faint,
                        topLeft = androidx.compose.ui.geometry.Offset(stroke / 2, stroke / 2),
                        size = androidx.compose.ui.geometry.Size(size.width - stroke, size.height - stroke),
                        cornerRadius = CornerRadius(10.dp.toPx()),
                        style = Stroke(stroke, pathEffect = PathEffect.dashPathEffect(floatArrayOf(6.dp.toPx(), 5.dp.toPx()))),
                    )
                },
            contentAlignment = Alignment.Center,
        ) {
            Icon(PodIcons.Plus, contentDescription = null, tint = colors.muted, modifier = Modifier.size(32.dp))
        }
        Spacer(Modifier.height(10.dp))
        Text("Add a podcast", style = PodType.rowTitle, color = colors.ink)
        Text("Search the directory or paste a feed", style = PodType.meta, color = colors.muted, maxLines = 2)
    }
}

