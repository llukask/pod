package ltd.kilian.pod.downloads

import android.system.ErrnoException
import android.system.OsConstants
import android.util.Log
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import java.net.ConnectException
import java.net.UnknownHostException
import java.util.concurrent.TimeUnit
import kotlin.coroutines.coroutineContext
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import ltd.kilian.pod.net.NetworkState
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response

// ==============================================================================================
// One transfer of one episode's audio (spec 7.2, 7.3)
// ==============================================================================================
//
// The reconciler owns the `download` rows: it moves a row to `transferring` under a fresh
// `run_id` and starts the transfer. From then on the transfer is the only writer of that row,
// and every write it makes carries `run_id = :run_id`. If a write matches no row, the row was
// deleted (wish removed, eviction, logout) or handed to a newer run, so the transfer deletes its
// own files and stops without re-creating anything.
//
// The transfer leaves the row in the state its outcome calls for (`complete`, `queued`,
// `waiting_retry` or `failed`). A cancelled transfer checkpoints and leaves the row
// `transferring`; what happens next is the canceller's call. Cancellation is checked between
// chunks.
//
// Byte progress for the UI goes to [Transfer.run]'s callback, in memory only. The row gets a
// durable checkpoint at most every [CHECKPOINT_EVERY_MS], after an fsync, so `bytes_done` never
// exceeds what is safely on disk. A port of pod-desktop's `download.rs`.

/** How often a running transfer writes `bytes_done` to its row (spec 7.3). */
const val CHECKPOINT_EVERY_MS = 5_000L

/** A transient error on this attempt fails the download for good (spec 7.2). */
const val MAX_ATTEMPTS = 8
private const val RETRY_BASE_MS = 30_000L
private const val RETRY_CAP_MS = 6 * 60 * 60_000L
private const val CHUNK = 64 * 1024
private const val TAG = "PodTransfer"

/** In-memory progress for the UI. */
data class Progress(val bytesDone: Long, val bytesTotal: Long?)

/** How a transfer ended. Every outcome but [Cancelled] and [LostOwnership] names the state the transfer left its row in. */
sealed interface Outcome {
    data object Complete : Outcome

    /** No connection or disk full. `attempts` is unchanged. */
    data class Queued(val reason: WaitReason) : Outcome
    data class WaitingRetry(val attempts: Int) : Outcome
    data object Failed : Outcome

    /** The row is still `transferring` with a fresh checkpoint. */
    data object Cancelled : Outcome

    /** The row was gone or belonged to another run. The transfer deleted its own files and wrote nothing. */
    data object LostOwnership : Outcome
}

/**
 * The client every download uses: a per-read timeout rather than a total one, since an
 * hour-long episode on a slow link is fine as long as bytes keep coming.
 */
fun downloadHttpClient(base: OkHttpClient): OkHttpClient = base.newBuilder()
    .connectTimeout(20, TimeUnit.SECONDS)
    .readTimeout(60, TimeUnit.SECONDS)
    .callTimeout(0, TimeUnit.SECONDS)
    .build()

/** The error classes of spec 7.2. */
private enum class ErrorClass { NoConnection, DiskFull, Transient, Permanent }

/** Why [Transfer.fetch] stopped short of publishing. */
private sealed class Stop : Exception() {
    class Error(val errorClass: ErrorClass, override val message: String) : Stop()
    object Cancelled : Stop()
    object LostOwnership : Stop()
}

private fun statusClass(status: Int) = when (status) {
    408, 429, in 500..599 -> ErrorClass.Transient
    // 404, 410 and every other 4xx, plus whatever a redirect chain left unresolved.
    else -> ErrorClass.Permanent
}

private fun localIo(error: IOException): Stop {
    val full = generateSequence<Throwable>(error) { it.cause }.any {
        (it is ErrnoException && (it.errno == OsConstants.ENOSPC || it.errno == OsConstants.EDQUOT)) || it.message?.contains("ENOSPC") == true
    }
    return Stop.Error(if (full) ErrorClass.DiskFull else ErrorClass.Transient, "local file: ${error.message}")
}

fun retryDelayMs(attempts: Int): Long = if (attempts >= 30) RETRY_CAP_MS else minOf(RETRY_BASE_MS shl attempts, RETRY_CAP_MS)

// `(first byte, complete length)` from a `Content-Range` such as `bytes 100-199/200` or
// `bytes */200`. Null parts are `*` or absent.
private fun contentRange(response: Response): Pair<Long?, Long?> {
    val value = response.header("Content-Range")?.removePrefix("bytes ") ?: return null to null
    val (range, total) = value.split('/', limit = 2).takeIf { it.size == 2 } ?: return null to null
    return range.substringBefore('-').toLongOrNull() to total.toLongOrNull()
}

/** The row's fields a transfer starts from. */
private class Claim(val audioUrl: String, val bytesDone: Long, val bytesTotal: Long?, val validator: String?, val podcastId: String, val audioType: String)

/** What every transfer shares. */
class Transfer(
    private val database: PodDatabase,
    private val client: OkHttpClient,
    /** Files live at `<root>/<podcast_id>/<episode_id>.<run_id>.<ext>[.part]`. */
    val root: File,
    /** A request error while the OS reports no connection is a "no connection" error, whatever OkHttp says it was. */
    private val network: () -> NetworkState,
    private val checkpointEveryMs: Long = CHECKPOINT_EVERY_MS,
    private val now: () -> Long = System::currentTimeMillis,
) {
    /** Runs the transfer for the row [episodeId] owned by [runId]. Throws only when the store itself fails; the row then stays `transferring` for the startup repair. */
    suspend fun run(episodeId: String, runId: String, onProgress: (Progress) -> Unit = {}): Outcome = withContext(Dispatchers.IO) {
        val claim = database.openHelper.writableDatabase.queryOne(
            """select d.audio_url, d.bytes_done, d.bytes_total, d.validator, e.podcast_id, e.audio_type
               from download d join episode e on e.id = d.episode_id
               where d.episode_id = ? and d.run_id = ?""",
            episodeId, runId,
        ) {
            Claim(it.string("audio_url"), maxOf(0, it.long("bytes_done")), it.longOrNull("bytes_total"), it.stringOrNull("validator"), it.string("podcast_id"), it.string("audio_type"))
        }
        // Without a row there is no path to clean up either. The startup sweep deletes files no
        // row accounts for.
            ?: return@withContext Outcome.LostOwnership
        val files = RunFiles.of(root, claim.podcastId, episodeId, runId, claim.audioType)
        if (files == null) {
            Log.e(TAG, "episode $episodeId has ids that aren't safe file names")
            return@withContext withContext(NonCancellable) { finish(Run(episodeId, runId, null), Stop.Error(ErrorClass.Permanent, "unsafe file name")) }
        }
        val run = Run(episodeId, runId, files)
        val stop = try {
            fetch(run, claim, onProgress)
            null
        } catch (stop: Stop) {
            stop
        }
        val outcome = withContext(NonCancellable) { finish(run, stop) }
        Log.i(TAG, "transfer of $episodeId ($runId) ended: $outcome")
        outcome
    }

    private class Run(val episodeId: String, val runId: String, val files: RunFiles?)

    /** Runs one guarded write that returns how many rows it changed, and turns zero into [Stop.LostOwnership]. */
    private fun writeOwned(run: Run, write: (androidx.sqlite.db.SupportSQLiteDatabase) -> Int) {
        val changed = database.runInTransaction<Int> { write(database.openHelper.writableDatabase) }
        if (changed == 0) throw Stop.LostOwnership
    }

    private fun networkError(error: IOException): Stop {
        val errorClass = when {
            error is UnknownHostException || error is ConnectException || !network().connected -> ErrorClass.NoConnection
            else -> ErrorClass.Transient
        }
        return Stop.Error(errorClass, error.message ?: error.javaClass.simpleName)
    }

    /** fsyncs, then records [done] as the durable checkpoint. */
    private fun checkpoint(run: Run, file: RandomAccessFile, done: Long) {
        try {
            file.fd.sync()
        } catch (e: IOException) {
            throw localIo(e)
        }
        writeOwned(run) { it.exec("update download set bytes_done = ? where episode_id = ? and run_id = ?", done, run.episodeId, run.runId) }
    }

    /**
     * Checkpoints before pausing on [stop], so the next attempt resumes from here. Losing the row
     * overrides [stop]; a failed checkpoint otherwise leaves the older one, which is still safe.
     */
    private fun checkpointThen(run: Run, file: RandomAccessFile, done: Long, stop: Stop): Stop = try {
        checkpoint(run, file, done)
        stop
    } catch (lost: Stop.LostOwnership) {
        lost
    } catch (other: Stop) {
        stop
    }

    /**
     * Drops the partial file and checkpoint after the server answered a range request with
     * something that can't continue it, then retries as a transient error.
     */
    private fun restartLater(run: Run, file: RandomAccessFile, reason: String): Nothing {
        try {
            file.setLength(0)
        } catch (e: IOException) {
            throw localIo(e)
        }
        writeOwned(run) { it.exec("update download set bytes_done = 0, validator = null where episode_id = ? and run_id = ?", run.episodeId, run.runId) }
        throw Stop.Error(ErrorClass.Transient, reason)
    }

    private suspend fun fetch(run: Run, claim: Claim, onProgress: (Progress) -> Unit) {
        val files = run.files!!
        val file = try {
            files.part.parentFile!!.mkdirs()
            RandomAccessFile(files.part, "rw")
        } catch (e: IOException) {
            throw localIo(e)
        }
        file.use {
            // Resume from the checkpoint, never from the file's length: past the checkpoint the
            // file may hold a torn tail (spec 7.3 step 1).
            val resume = claim.validator?.takeIf { claim.bytesDone > 0 && file.length() >= claim.bytesDone }
            val done = if (resume != null) claim.bytesDone else 0
            try {
                file.setLength(done)
                file.seek(done)
            } catch (e: IOException) {
                throw localIo(e)
            }

            // Byte offsets in `Range`, `Content-Range` and `bytes_done` count the encoded body, so
            // OkHttp's transparent gzip would make a response impossible to resume.
            val request = try {
                Request.Builder().url(claim.audioUrl).header("Accept-Encoding", "identity")
                    .apply { if (resume != null) header("Range", "bytes=$done-").header("If-Range", resume) }
                    .build()
            } catch (e: IllegalArgumentException) {
                throw Stop.Error(ErrorClass.Permanent, "not a URL: ${claim.audioUrl}")
            }
            val call = client.newCall(request)
            coroutineScope {
                // Cancelling the transfer cancels the call, which makes a blocked read throw at
                // once; the read loop then sees the cancellation and checkpoints.
                val watcher = launch(start = CoroutineStart.UNDISPATCHED) {
                    try {
                        awaitCancellation()
                    } finally {
                        call.cancel()
                    }
                }
                try {
                    val response = try {
                        call.execute()
                    } catch (e: IOException) {
                        if (!isActive) throw Stop.Cancelled
                        throw networkError(e)
                    }
                    response.use { read(run, claim, file, response, resume, done, onProgress) }
                } finally {
                    watcher.cancel()
                }
            }
        }
    }

    /** Reads the response into the `.part` file and publishes it. Returns the bytes written. */
    private suspend fun read(run: Run, claim: Claim, file: RandomAccessFile, response: Response, resume: String?, start: Long, onProgress: (Progress) -> Unit): Long {
        var done = start
        val status = response.code
        val type = response.body?.contentType()?.toString() ?: response.header("Content-Type")
        val html = type?.trimStart()?.lowercase()?.startsWith("text/html") == true
        // A login wall or an error page served with 200.
        if (response.isSuccessful && html) throw Stop.Error(ErrorClass.Permanent, "the server sent an HTML page instead of audio")
        val total: Long? = when {
            status == 206 && resume != null -> {
                val (first, length) = contentRange(response)
                if (first != done) restartLater(run, file, "206 for a different range")
                length
            }
            status == 200 -> {
                // No range, or `If-Range` saw a changed file.
                done = 0
                try {
                    file.setLength(0)
                    file.seek(0)
                } catch (e: IOException) {
                    throw localIo(e)
                }
                response.body?.contentLength()?.takeIf { it >= 0 }
            }
            status == 416 && resume != null -> {
                val length = contentRange(response).second ?: claim.bytesTotal
                if (length != done) restartLater(run, file, "416 for an incomplete file")
                // The checkpoint already covers the whole file.
                recordResponse(run, length, claim.validator, done)
                publish(run, file, done)
                return done
            }
            else -> throw Stop.Error(statusClass(status), "HTTP $status")
        }
        val validator = response.header("ETag") ?: response.header("Last-Modified")
        recordResponse(run, total, validator, done)

        val body = response.body?.byteStream() ?: throw Stop.Error(ErrorClass.Transient, "no body")
        val buffer = ByteArray(CHUNK)
        var lastCheckpoint = System.nanoTime()
        while (true) {
            onProgress(Progress(done, total))
            if (!coroutineContext.isActive) {
                checkpoint(run, file, done)
                throw Stop.Cancelled
            }
            val read = try {
                body.read(buffer)
            } catch (e: IOException) {
                if (!coroutineContext.isActive) {
                    checkpoint(run, file, done)
                    throw Stop.Cancelled
                }
                throw checkpointThen(run, file, done, networkError(e))
            }
            if (read < 0) break
            try {
                file.write(buffer, 0, read)
            } catch (e: IOException) {
                throw checkpointThen(run, file, done, localIo(e))
            }
            done += read
            if ((System.nanoTime() - lastCheckpoint) / 1_000_000 >= checkpointEveryMs) {
                checkpoint(run, file, done)
                lastCheckpoint = System.nanoTime()
            }
        }
        if (total != null && total != done) {
            throw checkpointThen(run, file, done, Stop.Error(ErrorClass.Transient, "the body ended after $done of $total bytes"))
        }
        publish(run, file, done)
        return done
    }

    /** Records what the response says about the file: its size on the row and in `known_size`, the validator for `If-Range`, and where this response starts writing. */
    private fun recordResponse(run: Run, total: Long?, validator: String?, done: Long) {
        writeOwned(run) { db ->
            val changed = db.exec(
                "update download set bytes_total = ?, validator = ?, bytes_done = ? where episode_id = ? and run_id = ?",
                total, validator, done, run.episodeId, run.runId,
            )
            if (total != null && changed > 0) {
                db.exec(
                    "insert into known_size (episode_id, bytes) values (?, ?) on conflict (episode_id) do update set bytes = excluded.bytes",
                    run.episodeId, total,
                )
            }
            changed
        }
    }

    /**
     * fsync, rename to the final name, then mark the row complete in one transaction (spec 7.2).
     * Readers find files through `download.path`, so the file only counts once that commits.
     */
    private fun publish(run: Run, file: RandomAccessFile, done: Long) {
        val files = run.files!!
        try {
            file.fd.sync()
        } catch (e: IOException) {
            throw localIo(e)
        }
        if (!files.part.renameTo(files.finished)) throw Stop.Error(ErrorClass.Transient, "local file: can't rename ${files.part.name}")
        writeOwned(run) {
            it.exec(
                """update download set state = 'complete', path = ?, completed_at = ?, bytes_done = ?, wait_reason = null, last_error = null
                   where episode_id = ? and run_id = ?""",
                files.relative, now(), done, run.episodeId, run.runId,
            )
        }
    }

    /** Moves the row out of `transferring` according to how [fetch] ended. */
    private fun finish(run: Run, stop: Stop?): Outcome {
        val error = when (stop) {
            null -> return Outcome.Complete
            Stop.Cancelled -> return Outcome.Cancelled
            Stop.LostOwnership -> {
                deleteFiles(run)
                return Outcome.LostOwnership
            }
            is Stop.Error -> stop
        }
        Log.w(TAG, "transfer of ${run.episodeId} stopped: ${error.errorClass} ${error.message}")
        val recorded = database.runInTransaction<Pair<Outcome, Boolean>?> { recordError(run, error.errorClass, error.message) }
        if (recorded == null) {
            deleteFiles(run)
            return Outcome.LostOwnership
        }
        val (outcome, keepPart) = recorded
        if (!keepPart) run.files?.part?.let(::removeFile)
        return outcome
    }

    private fun deleteFiles(run: Run) {
        run.files?.let {
            removeFile(it.part)
            removeFile(it.finished)
        }
    }

    /**
     * Writes the state an error class leads to (spec 7.2) and returns the outcome and whether the
     * `.part` file is worth keeping. Null if the row isn't this run's.
     */
    private fun recordError(run: Run, errorClass: ErrorClass, message: String): Pair<Outcome, Boolean>? {
        val db = database.openHelper.writableDatabase
        val (attempts, resumable) = db.queryOne(
            "select attempts, bytes_done > 0 and validator is not null as resumable from download where episode_id = ? and run_id = ?",
            run.episodeId, run.runId,
        ) { it.long("attempts").toInt() to (it.long("resumable") != 0L) } ?: return null
        val (outcome, keepPart) = when {
            errorClass == ErrorClass.NoConnection -> Outcome.Queued(WaitReason.Connection) to true
            errorClass == ErrorClass.DiskFull -> Outcome.Queued(WaitReason.Space) to true
            errorClass == ErrorClass.Transient && attempts + 1 < MAX_ATTEMPTS -> Outcome.WaitingRetry(attempts + 1) to true
            // Keep the `.part` only if a user retry could resume it.
            else -> Outcome.Failed to resumable
        }
        val state = when (outcome) {
            is Outcome.Queued -> "queued"
            is Outcome.WaitingRetry -> "waiting_retry"
            else -> "failed"
        }
        db.exec(
            """update download set state = ?, attempts = ?, next_attempt_at = ?, wait_reason = ?, last_error = ?,
                   bytes_done = case when ? then bytes_done else 0 end,
                   validator = case when ? then validator else null end
               where episode_id = ? and run_id = ?""",
            state,
            if (errorClass == ErrorClass.Transient) attempts + 1 else attempts,
            (outcome as? Outcome.WaitingRetry)?.let { now() + retryDelayMs(it.attempts) },
            (outcome as? Outcome.Queued)?.reason?.wire,
            message,
            keepPart,
            keepPart,
            run.episodeId,
            run.runId,
        )
        return outcome to keepPart
    }
}
