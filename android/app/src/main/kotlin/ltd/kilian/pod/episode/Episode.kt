package ltd.kilian.pod.episode

import androidx.sqlite.db.SupportSQLiteDatabase
import ltd.kilian.pod.inbox.displayTitle
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull
import ltd.kilian.pod.sync.EpisodeChange
import ltd.kilian.pod.sync.LocalChange

// The episode screen (wireframe M7): one episode with its state and show notes.

val EPISODE_TABLES = arrayOf("episode", "podcast", "subscription", "episode_state", "download", "download_wish")

data class EpisodeDetail(
    val id: String,
    val podcastId: String,
    val podcastTitle: String,
    val title: String,
    val publishedAt: Long,
    val durationMs: Long?,
    val positionMs: Long,
    val played: Boolean,
    val archived: Boolean,
    val link: String?,
    val artworkUrl: String?,
    /** `content_html` if the feed has one, else `summary_html`. Both are sanitized HTML (spec 2.2). */
    val showNotesHtml: String,
)

fun loadEpisode(db: SupportSQLiteDatabase, id: String): EpisodeDetail? =
    db.queryOne(
        """select e.id, e.podcast_id, p.title as podcast_title, p.local_title, coalesce(s.feed_url, p.feed_url) as feed_url,
                  e.title, e.published_at, e.duration_ms, e.link,
                  coalesce(e.artwork_url, p.artwork_url, p.local_artwork_url) as artwork_url,
                  case when e.content_html <> '' then e.content_html else e.summary_html end as notes,
                  coalesce(st.position_ms, 0) as position_ms, coalesce(st.played, 0) as played,
                  coalesce(st.archived, 0) as archived
           from episode e
           join podcast p on p.id = e.podcast_id
           left join subscription s on s.podcast_id = e.podcast_id
           left join episode_state st on st.episode_id = e.id
           where e.id = ?""",
        id,
    ) {
        EpisodeDetail(
            id = it.string("id"),
            podcastId = it.string("podcast_id"),
            podcastTitle = displayTitle(it.stringOrNull("podcast_title"), it.stringOrNull("local_title"), it.string("feed_url")),
            title = it.string("title"),
            publishedAt = it.long("published_at"),
            durationMs = it.longOrNull("duration_ms"),
            positionMs = it.long("position_ms"),
            played = it.flag("played"),
            archived = it.flag("archived"),
            link = it.stringOrNull("link"),
            artworkUrl = it.stringOrNull("artwork_url"),
            showNotesHtml = it.string("notes"),
        )
    }

/** The local write for marking an episode played or unplayed. */
fun played(episodeId: String, podcastId: String, played: Boolean) = LocalChange.Episode(episodeId, podcastId, EpisodeChange.Played(played))

/** The local write for archiving or unarchiving an episode. */
fun archived(episodeId: String, podcastId: String, archived: Boolean) = LocalChange.Episode(episodeId, podcastId, EpisodeChange.Archived(archived))
