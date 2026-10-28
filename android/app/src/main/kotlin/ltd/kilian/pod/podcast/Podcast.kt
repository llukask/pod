package ltd.kilian.pod.podcast

import androidx.sqlite.db.SupportSQLiteDatabase
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import ltd.kilian.pod.core.FeedError
import ltd.kilian.pod.core.PodcastStatus
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.core.message
import ltd.kilian.pod.inbox.displayTitle
import ltd.kilian.pod.inbox.timeLeftLabel
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull
import ltd.kilian.pod.sync.EpisodeChange
import ltd.kilian.pod.sync.LocalChange
import ltd.kilian.pod.sync.SyncHandle
import ltd.kilian.pod.sync.writeLocal

// ==============================================================================================
// A podcast's page (wireframes M5 and M6, spec 5.5)
// ==============================================================================================
//
// The query behind it, the wording of its states and of the unsubscribe confirmation, and the
// writes it makes. A port of pod-desktop's `podcast/mod.rs`.

/** The most episodes "keep newest N" offers (wireframe M6: Off, 1 to 10). */
const val AUTO_KEEP_MAX = 10

val PODCAST_TABLES = arrayOf("podcast", "episode", "subscription", "episode_state", "podcast_device", "download", "download_wish", "sync_scope")

/** A subscribed podcast with everything its page shows. */
data class PodcastPage(
    val id: String,
    val title: String,
    val author: String?,
    val description: String?,
    /** The subscription's URL, which the unsubscribe mutation carries. */
    val feedUrl: String,
    val state: PodcastState,
    /** Newest first, the podcast's whole history. */
    val episodes: List<PodcastEpisode>,
    val downloads: Downloads,
    val device: DeviceSettings,
    val artworkUrl: String?,
    /** The podcast scope is still backfilling ("Loading episodes… 240 so far"). */
    val loading: Boolean,
)

/** What the header shows about the feed, from the `podcast` row's `status`, `failing_since` and `feed_error`. */
sealed interface PodcastState {
    /** Not fetched yet: a placeholder (spec 5.5), or a subscription whose podcast row hasn't synced. */
    data object Pending : PodcastState
    data object Ok : PodcastState

    /** Fetches keep failing; the poller keeps trying. */
    data class Failing(val since: Long, val error: FeedError) : PodcastState

    /** The feed answered HTTP 410 and polling stopped. Episodes stay. */
    data object Gone : PodcastState
}

data class PodcastEpisode(
    val id: String,
    val title: String,
    val publishedAt: Long,
    val durationMs: Long?,
    val positionMs: Long,
    val played: Boolean,
    val archived: Boolean,
)

/** This podcast's `download` rows, which unsubscribing removes (spec 7.4). */
data class Downloads(
    /** Rows in any state: a queued pin goes as surely as a finished file. */
    val total: Int,
    /** The rows a `pin` wish covers. */
    val pinned: Int,
)

/** The podcast's `podcast_device` row (spec 7.1). */
data class DeviceSettings(
    /** New-episode notifications (7.6). */
    val notify: Boolean = false,
    /** Keep the newest N unplayed episodes downloaded, 0 for off. */
    val autoKeep: Int = 0,
    /** Auto downloads may use a metered network. */
    val autoOnMetered: Boolean = false,
)

// ==============================================================================================
// Wording
// ==============================================================================================

private fun plural(count: Int, one: String, many: String) = if (count == 1) "1 $one" else "$count $many"

/** How long a feed has been failing, in the coarsest unit that isn't zero: "a few minutes", "3 hours", "2 days". */
private fun failingForLabel(since: Long, now: Long): String {
    val hours = maxOf(0, now - since) / 3_600_000
    val days = hours / 24
    return when {
        days >= 1 -> plural(days.toInt(), "day", "days")
        hours >= 1 -> plural(hours.toInt(), "hour", "hours")
        else -> "a few minutes"
    }
}

/**
 * The banner of a failing or gone feed (M3 annotations 2 and 4), null for the others. A failing
 * feed names the reason and says the server keeps retrying. A gone feed's episodes and downloads
 * stay while the user stays subscribed, and the banner says so.
 */
fun statusNotice(state: PodcastState, episodes: Int, downloads: Int, now: Long): String? = when (state) {
    PodcastState.Pending, PodcastState.Ok -> null
    is PodcastState.Failing ->
        "${state.error.message}. Pod has kept trying for ${failingForLabel(state.since, now)} and will keep checking for new episodes."
    PodcastState.Gone -> {
        val kept = when {
            episodes == 0 && downloads == 0 -> null
            downloads == 0 && episodes == 1 -> "Your 1 episode is kept."
            downloads == 0 -> "Your $episodes episodes are kept."
            else -> "Your ${plural(episodes, "episode", "episodes")} and ${plural(downloads, "download", "downloads")} are kept."
        }
        listOfNotNull("The publisher removed this feed, and Pod has stopped checking for new episodes.", kept).joinToString(" ")
    }
}

/** The unsubscribe confirmation's message (M11). Unsubscribing removes every download of the show, pins included. */
fun unsubscribeMessage(downloads: Downloads): String {
    val removed = when {
        downloads.total == 0 -> null
        downloads.pinned == 0 -> "${plural(downloads.total, "downloaded episode", "downloaded episodes")} will be removed from this phone."
        downloads.pinned == downloads.total ->
            "${plural(downloads.total, "pinned download", "pinned downloads")} will be removed from this phone."
        else -> "${plural(downloads.total, "downloaded episode", "downloaded episodes")} will be removed from this phone, including ${downloads.pinned} you pinned."
    }
    return listOfNotNull(removed, "Your listening history stays.").joinToString(" ")
}

fun markAllPlayedTitle(unplayed: Int) = "Mark ${plural(unplayed, "episode", "episodes")} played?"

/** The row's progress line: "Played", "21 min left" or null for an unstarted episode. */
fun progressLabel(episode: PodcastEpisode): String? =
    if (episode.played) "Played" else timeLeftLabel(episode.durationMs, episode.positionMs)

/** "Auto: newest 3" for the header (M5 annotation 3), null when auto-download is off. */
fun autoLabel(device: DeviceSettings): String? = if (device.autoKeep == 0) null else "Auto: newest ${device.autoKeep}"

// ==============================================================================================
// Query
// ==============================================================================================

private fun podcastState(status: String?, failingSince: Long?, feedError: String?): PodcastState =
    when (PodcastStatus.entries.firstOrNull { it.wire == status }) {
        null, PodcastStatus.Pending -> PodcastState.Pending
        PodcastStatus.Ok -> PodcastState.Ok
        PodcastStatus.Gone -> PodcastState.Gone
        // The apply step stores a failing row's reason with it; a row without one reads as a
        // generic failure rather than breaking the page.
        PodcastStatus.Failing -> PodcastState.Failing(
            failingSince ?: 0,
            feedError?.let { runCatching { WireJson.decodeFromString(FeedError.serializer(), it) }.getOrNull() } ?: FeedError("unknown"),
        )
    }

/** The page of podcast [id], or null unless it is subscribed. */
fun loadPage(db: SupportSQLiteDatabase, id: String): PodcastPage? {
    data class Header(val page: PodcastPage)
    val header = db.queryOne(
        """select p.title, p.local_title, p.author, p.description, s.feed_url, p.status, p.failing_since, p.feed_error,
                  coalesce(d.notify, 0) as notify, coalesce(d.auto_keep, 0) as auto_keep,
                  coalesce(d.auto_on_metered, 0) as auto_on_metered,
                  coalesce(p.artwork_url, p.local_artwork_url) as artwork_url,
                  (select count(*) from sync_scope where scope = 'podcast:' || s.podcast_id and complete = 0) as loading
           from subscription s
           left join podcast p on p.id = s.podcast_id
           left join podcast_device d on d.podcast_id = s.podcast_id
           where s.podcast_id = ? and s.subscribed = 1""",
        id,
    ) {
        PodcastPage(
            id = id,
            title = displayTitle(it.stringOrNull("title"), it.stringOrNull("local_title"), it.string("feed_url")),
            author = it.stringOrNull("author"),
            description = it.stringOrNull("description"),
            feedUrl = it.string("feed_url"),
            state = podcastState(it.stringOrNull("status"), it.longOrNull("failing_since"), it.stringOrNull("feed_error")),
            episodes = emptyList(),
            downloads = Downloads(0, 0),
            device = DeviceSettings(it.flag("notify"), it.long("auto_keep").toInt(), it.flag("auto_on_metered")),
            artworkUrl = it.stringOrNull("artwork_url"),
            loading = it.long("loading") > 0,
        )
    } ?: return null

    val episodes = db.queryList(
        """select e.id, e.title, e.published_at, e.duration_ms, coalesce(st.position_ms, 0) as position_ms,
                  coalesce(st.played, 0) as played, coalesce(st.archived, 0) as archived
           from episode e
           left join episode_state st on st.episode_id = e.id
           where e.podcast_id = ?
           order by e.published_at desc, e.id desc""",
        id,
    ) {
        PodcastEpisode(
            it.string("id"), it.string("title"), it.long("published_at"), it.longOrNull("duration_ms"),
            it.long("position_ms"), it.flag("played"), it.flag("archived"),
        )
    }
    val downloads = db.queryOne(
        """select count(*) as total, count(w.episode_id) as pinned
           from download d
           join episode e on e.id = d.episode_id
           left join download_wish w on w.episode_id = d.episode_id and w.source = 'pin'
           where e.podcast_id = ?""",
        id,
    ) { Downloads(it.long("total").toInt(), it.long("pinned").toInt()) }!!
    return header.copy(episodes = episodes, downloads = downloads)
}

// ==============================================================================================
// Writes
// ==============================================================================================

/**
 * Writes this device's settings for podcast [id]. They are device-local (spec 7.1), so this is a
 * plain store write without a mutation; the reconciler follows `podcast_device` changes itself.
 */
suspend fun setDeviceSettings(database: PodDatabase, id: String, settings: DeviceSettings) = withContext(Dispatchers.IO) {
    database.runInTransaction {
        database.openHelper.writableDatabase.exec(
            """insert into podcast_device (podcast_id, notify, auto_keep, auto_on_metered) values (?, ?, ?, ?)
               on conflict (podcast_id) do update set
                   notify = excluded.notify, auto_keep = excluded.auto_keep, auto_on_metered = excluded.auto_on_metered""",
            id, settings.notify, settings.autoKeep, settings.autoOnMetered,
        )
    }
}

/** Marks every unplayed episode of podcast [id] played, in one local transaction. */
suspend fun markAllPlayed(database: PodDatabase, sync: SyncHandle, id: String) {
    val unplayed = withContext(Dispatchers.IO) {
        database.openHelper.readableDatabase.queryList(
            """select e.id from episode e left join episode_state st on st.episode_id = e.id
               where e.podcast_id = ? and coalesce(st.played, 0) = 0""",
            id,
        ) { it.string("id") }
    }
    writeLocal(database, sync, unplayed.map { LocalChange.Episode(it, id, EpisodeChange.Played(true)) })
}

/** Unsubscribes from podcast [id]. The apply step's scope rule removes its catalog rows once the subscription is synced away. */
suspend fun unsubscribe(database: PodDatabase, sync: SyncHandle, id: String, feedUrl: String) {
    writeLocal(database, sync, LocalChange.Subscription(id, feedUrl, subscribed = false))
}
