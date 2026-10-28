package ltd.kilian.pod.downloads

import android.util.Log
import androidx.sqlite.db.SupportSQLiteDatabase
import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.longOrNull
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull

// ==============================================================================================
// The reconciler's store and file steps (spec 7.4)
// ==============================================================================================
//
// A pass is three steps:
//
// 1. [load] reads the download tables into a [Snapshot] in one read transaction.
// 2. [plan] turns the snapshot and the [Conditions] into [Action]s.
// 3. [apply] writes the actions in one transaction, every row write guarded by the `run_id` the
//    snapshot saw. The executor ([Reconciler]) then does the file and job side: cancelling,
//    deleting a run's files and renaming a resumable `.part` to the new run's name
//    ([moveFiles]), and starting the transfer jobs.
//
// The statements are pod-desktop's (`reconcile.rs`), so the two clients stay easy to compare.
// Guarded writes put their `where` parameters last, so every statement binds in reading order.

private const val TAG = "PodDownloads"

/** Reads everything [plan] needs in one snapshot. */
fun load(db: SupportSQLiteDatabase): Snapshot {
    val podcasts = db.queryList("select podcast_id, auto_keep, auto_on_metered from podcast_device") {
        PodcastRule(it.string("podcast_id"), it.long("auto_keep").coerceIn(0, Int.MAX_VALUE.toLong()).toInt(), it.flag("auto_on_metered"))
    }
    val episodes = db.queryList(
        """select e.id, e.podcast_id, e.published_at, e.audio_bytes, e.audio_url,
               coalesce(st.played, 0) as played, st.played_at, coalesce(st.archived, 0) as archived
           from episode e left join episode_state st on st.episode_id = e.id
           where e.podcast_id in (select podcast_id from podcast_device where auto_keep > 0)
              or e.id in (select episode_id from download_wish)
              or e.id in (select episode_id from download)""",
    ) {
        PlanEpisode(
            id = it.string("id"),
            podcastId = it.string("podcast_id"),
            publishedAt = it.long("published_at"),
            audioBytes = it.longOrNull("audio_bytes")?.takeIf { bytes -> bytes >= 0 },
            audioUrl = it.string("audio_url"),
            played = it.flag("played"),
            playedAt = it.longOrNull("played_at"),
            archived = it.flag("archived"),
        )
    }.associateBy { it.id }
    // The table admits only the two sources; a row with another one is ignored.
    val wishes = db.queryList("select episode_id, source, created_at, metered_ok from download_wish") {
        Source.parse(it.string("source"))?.let { source -> Wish(it.string("episode_id"), source, it.long("created_at"), it.flag("metered_ok")) }
    }.filterNotNull()
    val downloads = db.queryList(
        """select episode_id, state, audio_url, bytes_done, next_attempt_at, wait_reason,
               unwanted_since, run_id
           from download""",
    ) {
        PlanDownload(
            episodeId = it.string("episode_id"),
            // A state this build doesn't know is treated like `failed`: kept while wanted,
            // deleted when not.
            state = State.parse(it.string("state")) ?: State.Failed,
            audioUrl = it.string("audio_url"),
            bytesDone = maxOf(0, it.long("bytes_done")),
            nextAttemptAt = it.longOrNull("next_attempt_at"),
            waitReason = it.stringOrNull("wait_reason")?.let(WaitReason::parse),
            unwantedSince = it.longOrNull("unwanted_since"),
            runId = it.stringOrNull("run_id"),
        )
    }
    val knownSizes = db.queryList("select episode_id, bytes from known_size") { it.string("episode_id") to maxOf(0, it.long("bytes")) }.toMap()
    return Snapshot(podcasts, episodes, wishes, downloads, knownSizes, loadSettings(db))
}

/** The device settings of spec 7.1. Values are JSON; one that is missing or doesn't parse as the right type reads as the default. */
fun loadSettings(db: SupportSQLiteDatabase): DownloadSettings {
    fun value(key: String): JsonPrimitive? =
        db.queryOne("select value from device_setting where key = ?", key) { it.string("value") }
            ?.let { runCatching { Json.parseToJsonElement(it) as? JsonPrimitive }.getOrNull() }
            ?.takeIf { !it.isString }
    val defaults = DownloadSettings()
    return DownloadSettings(
        budget = value(DOWNLOAD_BUDGET_BYTES)?.longOrNull?.takeIf { it >= 0 } ?: defaults.budget,
        onMetered = value(DOWNLOAD_ON_METERED)?.booleanOrNull ?: defaults.onMetered,
        graceMs = value(EVICT_GRACE_HOURS)?.doubleOrNull?.takeIf { it.isFinite() && it >= 0 }?.let(::hoursToMs) ?: defaults.graceMs,
    )
}

/** A run [apply] started: the episode, the run it replaces (whose `.part` it inherits) and its own. */
data class Started(val episodeId: String, val previousRun: String?, val run: String)

/** What [apply] leaves for the executor: work outside the database, only for actions whose guard matched. */
data class Applied(
    val starts: List<Started> = emptyList(),
    /** `(episode_id, run)` whose files to delete. */
    val deadRuns: List<Pair<String, String>> = emptyList(),
    /** Episodes whose live transfer to cancel. */
    val cancels: List<String> = emptyList(),
)

/**
 * Writes [actions] in order inside the caller's transaction. [newRun] names each started run.
 * Every row write ends in `episode_id = ? and run_id is ?` (`is` matches a null run too), with
 * the row's id and run bound last, and only matches leave work for the executor.
 */
fun apply(db: SupportSQLiteDatabase, actions: List<Action>, newRun: () -> String): Applied {
    val starts = mutableListOf<Started>()
    val deadRuns = mutableListOf<Pair<String, String>>()
    val cancels = mutableListOf<String>()
    fun guarded(row: RowRef, sql: String, vararg set: Any?): Boolean = db.exec(sql, *set, row.episodeId, row.runId) > 0
    fun dead(row: RowRef) {
        cancels += row.episodeId
        row.runId?.let { deadRuns += row.episodeId to it }
    }
    for (action in actions) {
        when (action) {
            is Action.AddWish -> db.exec(
                "insert into download_wish (episode_id, source, created_at) values (?, ?, ?) on conflict do nothing",
                action.episodeId, action.source.wire, action.createdAt,
            )
            is Action.RemoveWish -> db.exec("delete from download_wish where episode_id = ? and source = ?", action.episodeId, action.source.wire)
            is Action.Insert -> db.exec(
                "insert into download (episode_id, state, audio_url, path) values (?, 'queued', ?, '') on conflict do nothing",
                action.episodeId, action.audioUrl,
            )
            is Action.SetUnwanted -> guarded(action.row, "update download set unwanted_since = ? where episode_id = ? and run_id is ?", action.since)
            is Action.SetWait -> guarded(
                action.row,
                "update download set wait_reason = ? where state = 'queued' and episode_id = ? and run_id is ?",
                action.reason?.wire,
            )
            is Action.Requeue -> guarded(
                action.row,
                """update download set state = 'queued', next_attempt_at = null, wait_reason = null
                   where state in ('waiting_retry', 'transferring') and episode_id = ? and run_id is ?""",
            )
            // The old run's writes now miss, so a live transfer stops at its next write and
            // deletes its own `.part`.
            is Action.Reset -> if (
                guarded(
                    action.row,
                    """update download set state = 'queued', audio_url = ?, run_id = null,
                           path = '', bytes_total = null, bytes_done = 0, validator = null,
                           attempts = 0, next_attempt_at = null, last_error = null,
                           wait_reason = null, completed_at = null, unwanted_since = null
                       where episode_id = ? and run_id is ?""",
                    action.audioUrl,
                )
            ) {
                dead(action.row)
            }
            is Action.Pause -> if (
                guarded(
                    action.row,
                    "update download set state = 'queued', wait_reason = ? where state = 'transferring' and episode_id = ? and run_id is ?",
                    action.reason.wire,
                )
            ) {
                cancels += action.row.episodeId
            }
            is Action.Remove -> if (guarded(action.row, "delete from download where episode_id = ? and run_id is ?")) dead(action.row)
            is Action.Start -> {
                val run = newRun()
                if (
                    guarded(
                        action.row,
                        """update download set state = 'transferring', run_id = ?, wait_reason = null, next_attempt_at = null
                           where state = 'queued' and episode_id = ? and run_id is ?""",
                        run,
                    )
                ) {
                    starts += Started(action.row.episodeId, action.row.runId, run)
                }
            }
        }
    }
    return Applied(starts, deadRuns, cancels)
}

// ==============================================================================================
// Files
// ==============================================================================================

/**
 * A run's two file names. Both carry the run id, so two runs never share a `.part` and a stale
 * run can't rename over a newer run's file (spec 7.1).
 */
class RunFiles private constructor(
    val part: File,
    val finished: File,
    /** [finished] relative to the root, as `download.path` records it. */
    val relative: String,
) {
    companion object {
        /**
         * Null when an id isn't a safe file name component. Ids arrive from the server; one
         * holding `/` or `..` would point outside the downloads root.
         */
        fun of(root: File, podcastId: String, episodeId: String, runId: String, audioType: String): RunFiles? {
            val safe = listOf(podcastId, episodeId, runId).all { id -> id.isNotEmpty() && id.all { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '-' } }
            if (!safe) return null
            val name = "$episodeId.$runId.${extension(audioType)}"
            val finished = File(File(root, podcastId), name)
            return RunFiles(File(finished.path + ".part"), finished, "$podcastId/$name")
        }
    }
}

/** The file extension for an enclosure MIME type. Players sniff the content, so an unknown type only costs a less descriptive name. */
fun extension(audioType: String): String = when (audioType.substringBefore(';').trim().lowercase()) {
    "audio/mpeg", "audio/mp3", "audio/mpeg3", "audio/x-mpeg" -> "mp3"
    "audio/mp4", "audio/x-m4a", "audio/m4a", "audio/aac", "audio/x-aac" -> "m4a"
    "audio/ogg", "application/ogg" -> "ogg"
    "audio/opus" -> "opus"
    "audio/flac", "audio/x-flac" -> "flac"
    "audio/wav", "audio/x-wav", "audio/wave" -> "wav"
    "video/mp4" -> "mp4"
    "video/quicktime" -> "mov"
    else -> "audio"
}

/**
 * The files of one run, `.part` or finished, in any podcast directory. Both names start with
 * `<episode_id>.<run_id>.` (spec 7.1), and run ids are unique, so the prefix can't match another
 * run. Looking in every directory finds them even when the episode row, and with it the podcast
 * id, is already gone.
 */
private fun runFiles(root: File, episodeId: String, runId: String): List<File> {
    val prefix = "$episodeId.$runId."
    return root.listFiles().orEmpty()
        .flatMap { it.listFiles().orEmpty().asList() }
        .filter { it.name.startsWith(prefix) }
}

/** Deletes [file] if it is there. A file that won't go is left to the startup sweep. */
fun removeFile(file: File) {
    if (file.exists() && !file.delete()) Log.w(TAG, "can't delete download file $file")
}

/**
 * The file side of a pass: deletes dead runs' files and gives each started run the previous
 * run's `.part`, so it resumes from it (spec 7.2, `queued` → `transferring`). The previous run's
 * job is dead: a pass only starts rows without a live job.
 */
fun moveFiles(root: File, applied: Applied) {
    for ((episodeId, run) in applied.deadRuns) runFiles(root, episodeId, run).forEach(::removeFile)
    for ((episodeId, previous, run) in applied.starts) {
        if (previous == null) continue
        val oldPrefix = "$episodeId.$previous."
        for (file in runFiles(root, episodeId, previous)) {
            val rest = file.name.removePrefix(oldPrefix)
            if (!rest.endsWith(".part")) continue
            // On failure the transfer starts from zero, and the startup sweep deletes the old file.
            if (!file.renameTo(File(file.parentFile, "$episodeId.$run.$rest"))) Log.w(TAG, "can't hand $file to the next run")
        }
    }
}

// ==============================================================================================
// Startup repair (spec 7.4)
// ==============================================================================================

/** A `download` row as the startup repair reads it. */
data class RepairRow(
    val episodeId: String,
    val state: String,
    val path: String,
    val bytesTotal: Long?,
    val runId: String?,
    /** The episode's `(podcast_id, audio_type)`, which name the run's files. */
    val files: Pair<String, String>?,
)

fun repairRows(db: SupportSQLiteDatabase): List<RepairRow> = db.queryList(
    """select d.episode_id, d.state, d.path, d.bytes_total, d.run_id, e.podcast_id, e.audio_type
       from download d left join episode e on e.id = d.episode_id""",
) {
    val podcastId = it.stringOrNull("podcast_id")
    val audioType = it.stringOrNull("audio_type")
    RepairRow(
        it.string("episode_id"), it.string("state"), it.string("path"), it.longOrNull("bytes_total"), it.stringOrNull("run_id"),
        if (podcastId != null && audioType != null) podcastId to audioType else null,
    )
}

/** What the repair found on disk: finished runs to publish, and complete rows whose file is gone. */
data class Repairs(val published: List<Published>, val missing: List<Pair<String, String?>>) {
    data class Published(val episodeId: String, val run: String, val path: String, val size: Long)
}

/**
 * A run whose final file exists with the expected size crashed between its rename and its row
 * write: publish it. A complete row without its file is deleted; the next pass re-queues it if
 * it is still wanted.
 */
fun findRepairs(root: File, rows: List<RepairRow>): Repairs {
    val published = mutableListOf<Repairs.Published>()
    val missing = mutableListOf<Pair<String, String?>>()
    for (row in rows) {
        if (row.state == "transferring" && row.runId != null && row.files != null) {
            val files = RunFiles.of(root, row.files.first, row.episodeId, row.runId, row.files.second) ?: continue
            val size = files.finished.takeIf { it.isFile }?.length() ?: continue
            if (row.bytesTotal == null || row.bytesTotal == size) published += Repairs.Published(row.episodeId, row.runId, files.relative, size)
        } else if (row.state == "complete" && !File(root, row.path).isFile) {
            missing += row.episodeId to row.runId
        }
    }
    return Repairs(published, missing)
}

fun recordRepairs(db: SupportSQLiteDatabase, repairs: Repairs, now: Long) {
    for ((episodeId, run, path, size) in repairs.published) {
        db.exec(
            """update download set state = 'complete', path = ?, completed_at = ?, bytes_done = ?,
                   wait_reason = null, last_error = null
               where state = 'transferring' and episode_id = ? and run_id = ?""",
            path, now, size, episodeId, run,
        )
    }
    for ((episodeId, run) in repairs.missing) {
        db.exec("delete from download where state = 'complete' and episode_id = ? and run_id is ?", episodeId, run)
    }
}

/**
 * Deletes every file under [root] that no row accounts for: a `complete` row accounts for its
 * `path`, any other row with a run for its run's `.part`.
 */
fun sweep(root: File, rows: List<RepairRow>) {
    fun accountsFor(relative: String, name: String) = rows.any { row ->
        when {
            row.state == "complete" -> row.path == relative
            row.runId != null -> name.startsWith("${row.episodeId}.${row.runId}.") && name.endsWith(".part")
            else -> false
        }
    }
    for (entry in root.listFiles().orEmpty()) {
        if (!entry.isDirectory) {
            removeFile(entry)
            continue
        }
        for (file in entry.listFiles().orEmpty()) {
            if (file.isFile && !accountsFor("${entry.name}/${file.name}", file.name)) {
                Log.i(TAG, "deleting download file $file, which no row accounts for")
                removeFile(file)
            }
        }
    }
}
