package ltd.kilian.pod.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import ltd.kilian.pod.downloads.Status
import ltd.kilian.pod.inbox.durationLabel
import ltd.kilian.pod.inbox.timeLeftLabel

// ==============================================================================================
// An episode in a list (the inbox, a show's page)
// ==============================================================================================
//
// [cover, inbox only] [show name / title, up to two lines / meta] [play button with its ring].
// The meta line reads "Oct 21 · 43 min", "· 21 min left" or "· Played"; a list wide enough for it
// (720 dp) moves the date and length into aligned columns, like a track list. Played rows are
// muted, the playing episode's title takes the accent, and the episode open beside the list (wide
// layouts) is tinted.

/** What a row shows, from either list's query. */
data class RowEpisode(
    val id: String,
    val podcastId: String,
    val podcastTitle: String?,
    val title: String,
    val artworkUrl: String?,
    val durationMs: Long?,
    val positionMs: Long,
    val played: Boolean,
    val archived: Boolean,
)

/** The episode open in the detail pane beside a list, so the list can mark its row. */
val LocalOpenEpisode = staticCompositionLocalOf<String?> { null }

/** "Played", "21 min left" or the length. */
fun lengthLabel(durationMs: Long?, positionMs: Long, played: Boolean): String? = when {
    played -> "Played"
    else -> timeLeftLabel(durationMs, positionMs) ?: durationMs?.let(::durationLabel)
}

@Composable
fun EpisodeRow(
    episode: RowEpisode,
    /** The date label, or null under "Today" and "Yesterday", whose heading says it. */
    day: String?,
    showPodcast: Boolean,
    columns: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = podColors
    val player = LocalPlayer.current
    val now by player.state.collectAsState()
    val current = now?.episodeId == episode.id
    val selected = LocalOpenEpisode.current == episode.id
    // The playing episode's position comes from the player, so its ring moves while it plays.
    val position = if (current) now!!.positionMs else episode.positionMs
    val duration = (if (current) now!!.durationMs else null) ?: episode.durationMs
    val length = listOfNotNull(lengthLabel(duration, position, episode.played), if (episode.archived) "Archived" else null).joinToString(" · ")
    val download = LocalDownloadStatuses.current[episode.id]

    Row(
        modifier
            .fillMaxWidth()
            .padding(horizontal = LocalGutter.current - 12.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(if (selected) colors.accentSoft else Color.Transparent)
            .clickable(onClick = onClick)
            .padding(start = 12.dp, end = 8.dp, top = 12.dp, bottom = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (showPodcast) {
            Artwork(episode.artworkUrl, 52.dp, episode.podcastTitle)
            Spacer(Modifier.width(14.dp))
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            if (showPodcast && episode.podcastTitle != null) {
                Text(episode.podcastTitle, style = PodType.metaMedium, color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            Text(
                episode.title,
                style = if (episode.played) PodType.rowTitle.copy(fontWeight = FontWeight(450)) else PodType.rowTitle,
                color = when {
                    current -> colors.accentInk
                    episode.played -> colors.muted
                    else -> colors.ink
                },
                maxLines = 2, overflow = TextOverflow.Ellipsis,
            )
            if (!columns) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(listOfNotNull(day, length.ifEmpty { null }).joinToString(" · "), style = PodType.meta.tabular(), color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    DownloadMark(download, Modifier.padding(start = 6.dp))
                }
            }
        }
        if (columns) {
            Text(day.orEmpty(), style = PodType.meta.tabular(), color = colors.muted, maxLines = 1, modifier = Modifier.padding(start = 16.dp).width(104.dp))
            Row(Modifier.width(136.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(length, style = PodType.meta.tabular(), color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                DownloadMark(download, Modifier.padding(start = 6.dp))
            }
        }
        Spacer(Modifier.width(8.dp))
        PlayToggle(
            fraction = if (duration != null && duration > 0) position.toFloat() / duration else 0f,
            played = episode.played,
            current = current,
            playing = current && now!!.playing,
            description = (if (current && now!!.playing) "Pause " else "Play ") + episode.title,
            onClick = { if (current) player.toggle() else player.play(episode.id) },
        )
    }
}

/** A small download glyph after the meta line, for an episode with a download in any state. */
@Composable
fun DownloadMark(status: Status?, modifier: Modifier = Modifier) {
    status ?: return
    val colors = podColors
    val (icon, tint, label) = when (status) {
        is Status.Downloaded -> Triple(PodIcons.Downloaded, colors.accent, "Downloaded")
        is Status.Downloading -> Triple(PodIcons.Download, colors.accent, "Downloading")
        is Status.Queued -> Triple(PodIcons.Queued, colors.faint, "Queued")
        is Status.Retrying -> Triple(PodIcons.Retry, colors.faint, "Waiting to retry")
        is Status.Failed -> Triple(PodIcons.Alert, colors.danger, "Download failed")
        Status.Skipped -> Triple(PodIcons.Minus, colors.faint, "Skipped, storage budget")
    }
    Icon(icon, contentDescription = label, tint = tint, modifier = modifier.size(15.dp))
}
