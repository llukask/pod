package ltd.kilian.pod.inbox

import androidx.sqlite.db.SupportSQLiteDatabase
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.temporal.ChronoUnit
import java.util.Locale
import kotlinx.serialization.json.Json
import ltd.kilian.pod.core.FeedUrl
import ltd.kilian.pod.core.PodcastStatus
import ltd.kilian.pod.core.Settings
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull

// ==============================================================================================
// The inbox (spec 6)
// ==============================================================================================
//
// The query behind the inbox list and the podcast grid's unplayed counts, and the labels the
// lists show. A port of pod-desktop's `inbox/mod.rs`.

/** The tables the inbox and the podcast grid read. */
val INBOX_TABLES = arrayOf("podcast", "episode", "subscription", "episode_state", "setting", "podcast_device")

/**
 * Spec 6's filter, shared by the list and the counts so they can't disagree. It joins `podcast`
 * (as `p`) for the list's titles. The window start is the one parameter, in epoch ms. Ids are
 * stored lowercase and hyphenated, so the newest-episode tie-break compares them the way the
 * spec does. `conformance/inbox.json` pins the rules.
 */
private const val INBOX_FILTER = """
    from episode e
    join subscription s on s.podcast_id = e.podcast_id and s.subscribed = 1
    left join episode_state st on st.episode_id = e.id
    left join podcast p on p.id = e.podcast_id
    where coalesce(st.played, 0) = 0
      and coalesce(st.archived, 0) = 0
      and e.published_at >= ?
      and (
            e.published_at >= s.subscribed_at
         or e.id = (select e2.id from episode e2
                    where e2.podcast_id = e.podcast_id
                    order by e2.published_at desc, e2.id desc limit 1)
      )"""

/** An episode in the inbox, with what its row shows. */
data class InboxEpisode(
    val id: String,
    val podcastId: String,
    val podcastTitle: String,
    val title: String,
    val publishedAt: Long,
    val durationMs: Long?,
    val positionMs: Long,
    /** The episode's artwork, else the podcast's. */
    val artworkUrl: String?,
)

/** A subscribed podcast in the grid (wireframe M3). */
data class ShowTile(
    val id: String,
    val title: String,
    /** `pending` while the podcast row is a placeholder or not synced yet. */
    val status: PodcastStatus,
    /** How many of its episodes are in the inbox. */
    val unplayed: Int,
    val artworkUrl: String?,
    /** New-episode notifications are on for it on this device (7.6). */
    val notify: Boolean,
)

/** The inbox at [now], newest first. */
fun loadInbox(db: SupportSQLiteDatabase, now: Long): List<InboxEpisode> =
    db.queryList(
        """select e.id, e.podcast_id, p.title as podcast_title, p.local_title, s.feed_url, e.title,
                  e.published_at, e.duration_ms, coalesce(st.position_ms, 0) as position_ms,
                  coalesce(e.artwork_url, p.artwork_url, p.local_artwork_url) as artwork_url
           $INBOX_FILTER
           order by e.published_at desc, e.id desc""",
        windowStart(db, now),
    ) {
        InboxEpisode(
            id = it.string("id"),
            podcastId = it.string("podcast_id"),
            podcastTitle = displayTitle(it.stringOrNull("podcast_title"), it.stringOrNull("local_title"), it.string("feed_url")),
            title = it.string("title"),
            publishedAt = it.long("published_at"),
            durationMs = it.longOrNull("duration_ms"),
            positionMs = it.long("position_ms"),
            artworkUrl = it.stringOrNull("artwork_url"),
        )
    }

/**
 * What every screen calls a podcast: the feed's title, else the directory title the placeholder
 * was added under, else the feed's host (spec 5.5). The host, not the URL: a feed the server
 * never fetched has no other name, and the URL may carry credentials.
 */
fun displayTitle(title: String?, localTitle: String?, feedUrl: String): String = title ?: localTitle ?: FeedUrl.host(feedUrl)

/** The subscribed podcasts, newest episode first, with their inbox counts. */
fun loadPodcasts(db: SupportSQLiteDatabase, now: Long): List<ShowTile> =
    db.queryList(
        """with inbox as (select e.podcast_id $INBOX_FILTER)
           select s.podcast_id, p.title, p.local_title, s.feed_url, coalesce(p.status, 'pending') as status,
                  (select count(*) from inbox i where i.podcast_id = s.podcast_id) as unplayed,
                  coalesce(p.artwork_url, p.local_artwork_url) as artwork_url,
                  coalesce(d.notify, 0) as notify
           from subscription s
           left join podcast p on p.id = s.podcast_id
           left join podcast_device d on d.podcast_id = s.podcast_id
           where s.subscribed = 1
           order by (select max(e.published_at) from episode e where e.podcast_id = s.podcast_id) desc nulls last,
                    coalesce(p.title, p.local_title, s.feed_url) collate nocase""",
        windowStart(db, now),
    ) {
        ShowTile(
            id = it.string("podcast_id"),
            title = displayTitle(it.stringOrNull("title"), it.stringOrNull("local_title"), it.string("feed_url")),
            status = PodcastStatus.entries.first { status -> status.wire == it.string("status") },
            unplayed = it.long("unplayed").toInt(),
            artworkUrl = it.stringOrNull("artwork_url"),
            notify = it.long("notify") != 0L,
        )
    }

/** How far syncing has got while scopes are still loading, for the empty inbox after sign-in. */
data class Loading(val showsDone: Int, val showsTotal: Int, val episodes: Int)

/** Null once the user scope and every podcast scope are complete. */
fun loadLoading(db: SupportSQLiteDatabase): Loading? =
    db.queryOne(
        """select (select count(*) from sync_scope where scope = 'user' and complete = 1) as user_complete,
                  (select count(*) from sync_scope where scope <> 'user' and complete = 1) as done,
                  (select count(*) from sync_scope where scope <> 'user') as total,
                  (select count(*) from episode) as episodes""",
    ) {
        val done = it.long("user_complete") == 1L && it.long("done") == it.long("total")
        if (done) null else Loading(it.long("done").toInt(), it.long("total").toInt(), it.long("episodes").toInt())
    }

/** `now - inbox_window_days`, in epoch ms, with the synced setting read the way the server reads it. */
fun windowStart(db: SupportSQLiteDatabase, now: Long): Long {
    val stored = db.queryOne("select value from setting where key = ?", Settings.INBOX_WINDOW_DAYS) { it.string("value") }
    // Unparseable JSON is as unusable as a wrong type: the default applies.
    val parsed = stored?.let { runCatching { Json.parseToJsonElement(it) }.getOrNull() }
    return now - Settings.inboxWindowDays(parsed) * Settings.DAY_MS
}

// ==============================================================================================
// Labels
// ==============================================================================================

/** "30 s", "28 min", "1 h 4 min", "2 h". Under a minute it counts seconds, otherwise it rounds to the minute. */
fun durationLabel(ms: Long): String {
    val seconds = (ms + 500) / 1_000
    if (seconds < 60) return "${maxOf(seconds, 1)} s"
    val minutes = (ms + 30_000) / 60_000
    return when {
        minutes < 60 -> "$minutes min"
        minutes % 60 == 0L -> "${minutes / 60} h"
        else -> "${minutes / 60} h ${minutes % 60} min"
    }
}

/** "21 min left" for a started episode of known length. */
fun timeLeftLabel(durationMs: Long?, positionMs: Long): String? {
    val remaining = (durationMs ?: return null) - positionMs
    return if (positionMs > 0 && remaining > 0) "${durationLabel(remaining)} left" else null
}

/** How far into a started episode of known length playback is, 0 to 1, or null. */
fun progress(durationMs: Long?, positionMs: Long): Float? =
    if (durationMs == null || durationMs <= 0 || positionMs <= 0) null else (positionMs.toFloat() / durationMs).coerceIn(0f, 1f)

/**
 * When an episode came out, as the lists show it: "Today", "Yesterday", a weekday within the
 * last week, else the date, with the year if it isn't this one. Days are calendar days in [zone].
 */
fun dayLabel(published: Long, now: Long, zone: ZoneId, locale: Locale = Locale.getDefault()): String {
    val day = Instant.ofEpochMilli(published).atZone(zone).toLocalDate()
    val today = Instant.ofEpochMilli(now).atZone(zone).toLocalDate()
    val daysAgo = ChronoUnit.DAYS.between(day, today)
    return when {
        daysAgo == 0L -> "Today"
        daysAgo == 1L -> "Yesterday"
        daysAgo in 2..6 -> DateTimeFormatter.ofPattern("EEE", locale).format(day)
        day.year == today.year -> DateTimeFormatter.ofPattern("MMM d", locale).format(day)
        else -> DateTimeFormatter.ofPattern("MMM d, yyyy", locale).format(day)
    }
}
