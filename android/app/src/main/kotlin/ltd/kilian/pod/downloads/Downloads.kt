package ltd.kilian.pod.downloads

import androidx.sqlite.db.SupportSQLiteDatabase
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.withContext
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull
import ltd.kilian.pod.sync.EpisodeChange
import ltd.kilian.pod.sync.LocalChange
import ltd.kilian.pod.sync.SyncHandle
import ltd.kilian.pod.sync.applyLocalChange
import ltd.kilian.pod.sync.newMutationId

// ==============================================================================================
// The downloads model (wireframes M7 and M9, spec 7.4)
// ==============================================================================================
//
// Every episode a download row or a wish names, the state each one is in, and the actions that
// get out of it. The UI only writes `download_wish`, `download` and the episode's archived flag;
// the reconciler does the rest. [status] and [actions] are pure, so each state's label and way
// out are unit-tested. A port of pod-desktop's `downloads/mod.rs`.

/** One episode's download, as loaded. */
data class Entry(
    val episodeId: String,
    val podcastId: String,
    val title: String,
    val podcastTitle: String,
    val pinned: Boolean,
    val auto: Boolean,
    /** Null for a wish the reconciler hasn't queued: a new one, or an auto wish skipped for the budget. */
    val row: Row?,
    /** Exact from the row or `known_size`, else the feed's `audio_bytes`. */
    val size: Size?,
)

data class Size(val bytes: Long, val exact: Boolean)

/** The `download` columns the UI shows. */
data class Row(
    val state: State,
    val waitReason: WaitReason? = null,
    val attempts: Int = 0,
    val nextAttemptAt: Long? = null,
    val lastError: String? = null,
    val bytesDone: Long = 0,
    val bytesTotal: Long? = null,
    /** When the grace period of an unwanted finished file ends. */
    val leavingAt: Long? = null,
    /** The finished file, relative to [Reconciler.root]. */
    val path: String = "",
)

/** What an episode's download is doing. */
sealed interface Status {
    /** An auto wish budget fitting left out. */
    data object Skipped : Status
    data class Queued(val reason: WaitReason?) : Status
    data class Downloading(val done: Long, val total: Long?) : Status

    /** [attempts] transient failures so far; the next try is due [at]. */
    data class Retrying(val at: Long?, val attempts: Int, val error: String?) : Status
    data class Failed(val error: String?) : Status

    /** [leavingAt] is set while the file waits out its grace period. */
    data class Downloaded(val leavingAt: Long?) : Status
}

/** A way out of a state. */
enum class Act(val label: String) {
    /** Pins an episode nothing wishes yet (M7's download button). */
    Download("Download"),

    /** Stops a download that hasn't finished. */
    Cancel("Cancel"),

    /** Deletes a failed row or a finished file. */
    Remove("Remove"),

    /** A failed row back to the queue. */
    Retry("Retry"),

    /** Pins the episode with `metered_ok` (spec 7.1). */
    UseMetered("Download on mobile data anyway"),

    /** Pins a skipped or budget-held auto download. */
    DownloadAnyway("Download anyway"),

    /** Pins a finished file in its grace period. */
    Keep("Keep"),

    /** Archives a skipped auto download, so the rule stops picking it. */
    Archive("Archive"),
}

/** The status of [entry], with live byte progress where a transfer runs. */
fun status(entry: Entry, fit: Fit, progress: Map<String, Progress>): Status {
    val row = entry.row ?: return if (entry.episodeId in fit.skipped) Status.Skipped else Status.Queued(null)
    return when (row.state) {
        State.Queued -> Status.Queued(row.waitReason)
        State.Transferring -> {
            val live = progress[entry.episodeId]
            Status.Downloading(live?.bytesDone ?: row.bytesDone, live?.bytesTotal ?: row.bytesTotal)
        }
        State.WaitingRetry -> Status.Retrying(row.nextAttemptAt, row.attempts, row.lastError)
        State.Failed -> Status.Failed(row.lastError)
        State.Complete -> Status.Downloaded(row.leavingAt)
    }
}

/** Every state's way out (M9: every row has one). */
fun actions(status: Status): List<Act> = when (status) {
    Status.Skipped -> listOf(Act.DownloadAnyway, Act.Archive)
    is Status.Queued -> when (status.reason) {
        WaitReason.Wifi -> listOf(Act.UseMetered, Act.Cancel)
        // Pins never wait for the budget, so this is an auto download.
        WaitReason.Budget -> listOf(Act.DownloadAnyway, Act.Cancel)
        else -> listOf(Act.Cancel)
    }
    is Status.Downloading, is Status.Retrying -> listOf(Act.Cancel)
    is Status.Failed -> listOf(Act.Retry, Act.Remove)
    is Status.Downloaded -> if (status.leavingAt != null) listOf(Act.Keep, Act.Remove) else listOf(Act.Remove)
}

/** The status text at [now] (epoch ms). */
fun statusLabel(status: Status, now: Long): String = when (status) {
    Status.Skipped -> "Skipped · doesn't fit the budget"
    is Status.Queued -> when (status.reason) {
        null -> "Queued"
        WaitReason.Connection -> "Waiting · no connection"
        WaitReason.Wifi -> "Waiting · metered network"
        WaitReason.Space -> "Paused · storage full"
        WaitReason.Budget -> "Waiting · doesn't fit the budget"
    }
    // TODO: the transfer rate, as in M9.
    is Status.Downloading -> when {
        status.total != null -> "Downloading · ${formatBytes(status.done)} of ${formatBytes(status.total)}"
        status.done == 0L -> "Downloading"
        else -> "Downloading · ${formatBytes(status.done)}"
    }
    is Status.Retrying -> {
        val left = status.at?.let { it - now }?.takeIf { it > 0 }
        val whenText = if (left != null) "Retrying in ${timeLabel(left)}" else "Retrying now"
        val error = status.error?.let { " · $it" }.orEmpty()
        "$whenText$error (attempt ${status.attempts + 1} of $MAX_ATTEMPTS)"
    }
    is Status.Failed -> status.error?.let { "Failed · $it" } ?: "Failed"
    is Status.Downloaded -> when {
        status.leavingAt == null -> "Downloaded"
        status.leavingAt > now -> "Downloaded · removed in ${timeLabel(status.leavingAt - now)}"
        else -> "Downloaded · removing"
    }
}

/** `45 s`, `3 min`, `20 h`, rounded up, for a positive span in ms. */
private fun timeLabel(ms: Long): String {
    val seconds = (maxOf(0, ms) + 999) / 1000
    return when {
        seconds < 60 -> "$seconds s"
        seconds < 3600 -> "${(seconds + 59) / 60} min"
        else -> "${(seconds + 3599) / 3600} h"
    }
}

/** What keeps the episode on this device. */
fun sourceLabel(entry: Entry): String = when {
    entry.pinned -> "Pinned"
    entry.auto -> "Auto"
    else -> "–"
}

/** The size. `~` marks the feed's own claim. */
fun sizeLabel(entry: Entry): String = when (val size = entry.size) {
    null -> "–"
    else -> if (size.exact) formatBytes(size.bytes) else "~${formatBytes(size.bytes)}"
}

/** Bytes in decimal units, one decimal: `48.0 MB`. The same wording as pod-desktop's `format_bytes`. */
fun formatBytes(bytes: Long): String {
    if (bytes < 1000) return "$bytes bytes"
    val units = listOf("KB", "MB", "GB", "TB")
    var value = bytes / 1000.0
    var unit = 0
    while (value >= 999.95 && unit < units.size - 1) {
        value /= 1000.0
        unit += 1
    }
    return "%.1f %s".format(java.util.Locale.ROOT, value, units[unit])
}

// ==============================================================================================
// Loading
// ==============================================================================================

/** The tables [loadEntries] reads. */
val ENTRY_TABLES = arrayOf("podcast", "episode", "download", "download_wish", "known_size", "device_setting")

/** Every episode a download row or a wish names, newest first. */
fun loadEntries(db: SupportSQLiteDatabase): List<Entry> {
    val graceMs = loadSettings(db).graceMs
    return db.queryList(
        """with ids as (select episode_id from download union select episode_id from download_wish)
           select e.id, e.podcast_id, e.title, coalesce(p.title, p.local_title, '') as podcast_title,
               exists(select 1 from download_wish w where w.episode_id = e.id and w.source = 'pin') as pinned,
               exists(select 1 from download_wish w where w.episode_id = e.id and w.source = 'auto') as auto,
               d.state, d.wait_reason, d.attempts, d.next_attempt_at, d.last_error,
               d.bytes_done, d.bytes_total, d.unwanted_since, d.path,
               coalesce(case when d.state = 'complete' then d.bytes_done end, d.bytes_total, ks.bytes) as exact,
               e.audio_bytes
           from ids join episode e on e.id = ids.episode_id
           left join podcast p on p.id = e.podcast_id
           left join download d on d.episode_id = e.id
           left join known_size ks on ks.episode_id = e.id
           order by e.published_at desc, e.id desc""",
    ) {
        val state = it.stringOrNull("state")
        val row = state?.let { text ->
            Row(
                // An unknown state reads as failed, as in the reconciler.
                state = State.parse(text) ?: State.Failed,
                waitReason = it.stringOrNull("wait_reason")?.let(WaitReason::parse),
                attempts = it.long("attempts").toInt(),
                nextAttemptAt = it.longOrNull("next_attempt_at"),
                lastError = it.stringOrNull("last_error"),
                bytesDone = maxOf(0, it.long("bytes_done")),
                bytesTotal = it.longOrNull("bytes_total"),
                leavingAt = it.longOrNull("unwanted_since")?.takeIf { text == "complete" }?.let { since -> since + graceMs },
                path = it.string("path"),
            )
        }
        Entry(
            episodeId = it.string("id"),
            podcastId = it.string("podcast_id"),
            title = it.string("title"),
            podcastTitle = it.string("podcast_title"),
            pinned = it.flag("pinned"),
            auto = it.flag("auto"),
            row = row,
            size = it.longOrNull("exact")?.let { bytes -> Size(bytes, true) } ?: it.longOrNull("audio_bytes")?.let { bytes -> Size(bytes, false) },
        )
    }
}

/** [loadEntries], again after every change to its tables. */
fun observeEntries(database: PodDatabase): Flow<List<Entry>> =
    database.invalidationTracker.createFlow(*ENTRY_TABLES).map {
        withContext(Dispatchers.IO) { loadEntries(database.openHelper.readableDatabase) }
    }

/**
 * Each wished or downloaded episode's [Status], by episode id, with live progress: what the inbox
 * and episode rows show as their download glyph. An episode missing from the map has no download.
 */
fun observeStatuses(database: PodDatabase, reconciler: Reconciler): Flow<Map<String, Status>> =
    combine(observeEntries(database), reconciler.fit, reconciler.progress) { entries, fit, progress ->
        entries.associate { it.episodeId to status(it, fit, progress) }
    }

// ==============================================================================================
// Writes
// ==============================================================================================

/** What [perform] took away from an auto download, for its "Archived and removed" undo. */
data class Removed(
    val episodeId: String,
    val podcastId: String,
    internal val wishes: List<Wish>,
)

/**
 * Does [act] for an episode and wakes the reconciler. Returns what to offer an undo for when the
 * episode was archived ("Archived and removed", spec 7.4).
 */
suspend fun perform(database: PodDatabase, sync: SyncHandle, reconciler: Reconciler, episodeId: String, podcastId: String, act: Act): Removed? {
    val removed = when (act) {
        Act.Cancel, Act.Remove, Act.Archive -> remove(database, sync, episodeId, podcastId)
        Act.Retry -> {
            write(database) {
                it.exec(
                    """update download set state = 'queued', attempts = 0, last_error = null, next_attempt_at = null, wait_reason = null
                       where episode_id = ? and state = 'failed'""",
                    episodeId,
                )
            }
            null
        }
        Act.UseMetered -> {
            pin(database, episodeId, meteredOk = true)
            null
        }
        Act.Download, Act.DownloadAnyway, Act.Keep -> {
            pin(database, episodeId, meteredOk = false)
            null
        }
    }
    reconciler.wake()
    return removed
}

private suspend fun <T> write(database: PodDatabase, block: (SupportSQLiteDatabase) -> T): T = withContext(Dispatchers.IO) {
    database.runInTransaction<T> { block(database.openHelper.writableDatabase) }
}

/**
 * Pins [episodeId] (spec 7.1). [meteredOk] also lets that one pin use a metered network; an
 * existing pin keeps its `created_at`, so a finished pin's release (spec 7.4) still dates from
 * when it was made.
 */
suspend fun pin(database: PodDatabase, episodeId: String, meteredOk: Boolean) = write(database) {
    it.exec(
        """insert into download_wish (episode_id, source, created_at, metered_ok) values (?, 'pin', ?, ?)
           on conflict (episode_id, source) do update set metered_ok = max(metered_ok, excluded.metered_ok)""",
        episodeId, System.currentTimeMillis(), meteredOk,
    )
}

/**
 * Drops every wish for the episode, and with them the row. A finished file gets
 * `unwanted_since = 0`, so the next pass deletes it instead of keeping it for the grace period.
 * When an auto wish covered the episode, it is archived in the same transaction, or the next
 * pass would wish it again (spec 7.4); a pin alongside goes too, since the user asked for the
 * download to stop.
 */
private suspend fun remove(database: PodDatabase, sync: SyncHandle, episodeId: String, podcastId: String): Removed? {
    val now = System.currentTimeMillis()
    val removed = write(database) { db ->
        val wishes = db.queryList("select source, created_at, metered_ok from download_wish where episode_id = ?", episodeId) {
            Source.parse(it.string("source"))?.let { source -> Wish(episodeId, source, it.long("created_at"), it.flag("metered_ok")) }
        }.filterNotNull()
        db.exec("delete from download_wish where episode_id = ?", episodeId)
        db.exec("update download set unwanted_since = 0 where episode_id = ? and state = 'complete'", episodeId)
        if (wishes.none { it.source == Source.Auto }) return@write null
        sync.inFlight.withLock { inFlight ->
            applyLocalChange(db, LocalChange.Episode(episodeId, podcastId, EpisodeChange.Archived(true)), newMutationId(now), inFlight, now)
        }
        Removed(episodeId, podcastId, wishes)
    }
    if (removed != null) sync.requestSync(LocalChange.Episode(episodeId, podcastId, EpisodeChange.Archived(true)).syncDelayMs)
    return removed
}

/** Undoes "Archived and removed": unarchives the episode and puts its wishes back, so the reconciler downloads it again. */
suspend fun undo(database: PodDatabase, sync: SyncHandle, reconciler: Reconciler, removed: Removed) {
    val now = System.currentTimeMillis()
    val change = LocalChange.Episode(removed.episodeId, removed.podcastId, EpisodeChange.Archived(false))
    write(database) { db ->
        for (wish in removed.wishes) {
            db.exec(
                "insert into download_wish (episode_id, source, created_at, metered_ok) values (?, ?, ?, ?) on conflict do nothing",
                wish.episodeId, wish.source.wire, wish.createdAt, wish.meteredOk,
            )
        }
        sync.inFlight.withLock { inFlight -> applyLocalChange(db, change, newMutationId(now), inFlight, now) }
    }
    sync.requestSync(change.syncDelayMs)
    reconciler.wake()
}
