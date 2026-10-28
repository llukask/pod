package ltd.kilian.pod.downloads

import android.os.StatFs
import android.util.Log
import androidx.sqlite.db.SupportSQLiteDatabase
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import ltd.kilian.pod.net.NetworkState
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.sync.newMutationId

// ==============================================================================================
// The download reconciler (spec 7.4)
// ==============================================================================================
//
// Decides which episodes should be on disk and starts, pauses, cancels and deletes transfers to
// match. Each pass is [load], [plan] and [apply] (see `Reconcile.kt`), then the job side.
//
// Where the work runs on Android, as spec 7.4 lays out: each transfer is a unique WorkManager
// job named `download:<episode_id>` ([TransferJobs], `DownloadWorker`), expedited for pins, with
// `NetworkType.CONNECTED`. WorkManager rather than a coroutine of the app's own: a job outlives
// the activity and the process, so a pin queued on a plane downloads when the plane lands even if
// Android killed the app in between, and a job the system stops (constraints lost, its execution
// window over) is rescheduled and resumes from its checkpoint. The passes themselves run in the
// app process on [run]'s loop, which every process start begins, WorkManager's included, so a
// background sync's end still triggers one.
//
// Passes hold [passLock], so only one runs at a time, and [stopped] holds it to keep passes out
// during logout. A port of pod-desktop's `reconcile.rs`.

/** The longest a pass waits for a deadline (a retry, the end of a grace period); spec 7.4's "every 15 minutes while any row is `waiting_retry`". */
private const val MAX_SLEEP_MS = 15 * 60_000L

/** How long the loop waits after a trigger, so a burst (a sync touching several tables, two transfers ending together) shares one pass. */
private const val DEBOUNCE_MS = 200L

/** After a failed pass, try again this soon even without a trigger. */
private const val RETRY_AFTER_ERROR_MS = 60_000L

private const val TAG = "PodReconciler"

/** Starts and stops transfer jobs. The production one is [WorkManagerJobs]; tests run transfers as coroutines. */
interface TransferJobs {
    /** Starts the job for [episodeId]. The job reads its run from the row, which the pass left `transferring`. */
    fun start(episodeId: String, expedited: Boolean)

    fun cancel(episodeId: String)

    fun cancelAll()

    /** Episodes whose job is queued or running. */
    suspend fun live(): Set<String>
}

class Reconciler(
    private val database: PodDatabase,
    /** Runs one transfer; its root is the downloads root. */
    val transfer: Transfer,
    private val network: StateFlow<NetworkState>,
    private val jobs: TransferJobs,
    private val freeSpace: (File) -> Long = { StatFs(it.path).availableBytes },
    private val now: () -> Long = System::currentTimeMillis,
) {
    /** The downloads root. Files are `<root>/<download.path>`. */
    val root: File get() = transfer.root

    private val passLock = Mutex()
    private val wakeups = Channel<Unit>(Channel.CONFLATED)
    private val playing = MutableStateFlow<String?>(null)

    private val fitState = MutableStateFlow(Fit())

    /** What the last pass's budget fitting left out, for the downloads page. */
    val fit: StateFlow<Fit> = fitState.asStateFlow()

    private val progressState = MutableStateFlow<Map<String, Progress>>(emptyMap())

    /** The running transfers' byte progress, by episode id; kept in memory only (spec 7.2). An entry goes when its transfer ends. */
    val progress: StateFlow<Map<String, Progress>> = progressState.asStateFlow()

    /**
     * Episodes whose transfer is running in this process. WorkManager marks a cancelled job
     * finished at once, while its worker may still be writing, so a job counts as live until its
     * transfer has returned too; a second run would otherwise take over its `.part`.
     */
    private val running = MutableStateFlow<Set<String>>(emptySet())

    private suspend fun live(): Set<String> = jobs.live() + running.value

    /**
     * Tells the reconciler which episode is playing, so its file is kept (spec 7.4). Every change
     * triggers a pass, so null on stop releases the file to its grace period: the "playback stop"
     * trigger. The player calls it on start, episode change and stop.
     */
    fun setPlaying(episodeId: String?) {
        playing.value = episodeId
    }

    /** Runs a pass soon. For writes to `download`, which the loop doesn't watch. */
    fun wake() {
        wakeups.trySend(Unit)
    }

    // ==========================================================================================
    // Passes
    // ==========================================================================================

    private suspend fun <T> transaction(block: (SupportSQLiteDatabase) -> T): T = withContext(Dispatchers.IO) {
        database.runInTransaction<T> { block(database.openHelper.writableDatabase) }
    }

    /** One pass. Returns when the next one is due without a trigger (epoch ms). */
    suspend fun pass(): Long? = passLock.withLock {
        val freeBytes = withContext(Dispatchers.IO) {
            root.mkdirs()
            freeSpace(root)
        }
        val conditions = Conditions(now(), network.value, freeBytes, playing.value, live())
        val snapshot = transaction(::load)
        val plan = plan(snapshot, conditions)
        if (plan.actions.isNotEmpty()) {
            Log.d(TAG, "download pass: ${plan.actions}")
            val applied = transaction { apply(it, plan.actions) { newMutationId(now()) } }
            applied.cancels.forEach(jobs::cancel)
            withContext(Dispatchers.IO) { moveFiles(root, applied) }
            val pinned = snapshot.wishes.filter { it.source == Source.Pin }.map { it.episodeId }.toSet()
            applied.starts.forEach { jobs.start(it.episodeId, expedited = it.episodeId in pinned) }
        }
        fitState.value = plan.fit
        plan.wakeAt
    }

    /**
     * Repairs crash leftovers (spec 7.4), then runs a pass on every trigger: the end of a sync
     * loop ([loopEnded]), wish and auto-download changes, played and archived flags, device
     * settings, connectivity, the playing episode, a transfer's end, [wake], and the plan's own
     * deadlines. Runs until cancelled.
     */
    suspend fun run(loopEnded: Flow<Unit>): Unit = coroutineScope {
        try {
            repair()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Log.e(TAG, "download startup repair failed", e)
        }
        launch { loopEnded.collect { wake() } }
        // Not `download`: transfers checkpoint into it every few seconds, and a transfer's end
        // wakes the loop itself.
        // TODO: position saves during playback touch `episode_state` every 5 s and cost a pass
        // each; watch only the played and archived columns if that shows up.
        launch {
            database.invalidationTracker
                .createFlow("download_wish", "podcast_device", "episode_state", "device_setting", emitInitialState = false)
                .collect { wake() }
        }
        launch { network.drop(1).collect { wake() } }
        launch { playing.drop(1).collect { wake() } }
        while (true) {
            val wakeAt = try {
                pass()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                Log.e(TAG, "download pass failed", e)
                now() + RETRY_AFTER_ERROR_MS
            }
            if (wakeAt == null) {
                wakeups.receive()
            } else {
                withTimeoutOrNull((wakeAt - now()).coerceIn(0, MAX_SLEEP_MS)) { wakeups.receive() }
            }
            delay(DEBOUNCE_MS)
            wakeups.tryReceive()
        }
    }

    /**
     * Repairs what a crash left behind (spec 7.4). A `transferring` row whose job survived the
     * process (WorkManager keeps it) stays as it is and resumes; the first pass requeues the
     * others, since none of them has a live job.
     */
    internal suspend fun repair() = passLock.withLock {
        val rows = transaction(::repairRows)
        val repairs = withContext(Dispatchers.IO) { findRepairs(root, rows) }
        transaction { recordRepairs(it, repairs, now()) }
        val repaired = transaction(::repairRows)
        withContext(Dispatchers.IO) { sweep(root, repaired) }
    }

    // ==========================================================================================
    // Transfers
    // ==========================================================================================

    /**
     * Runs the transfer for [episodeId]'s current run, for its job. Reports byte progress, and
     * wakes the loop when it ends, however it ends.
     */
    suspend fun runTransfer(episodeId: String): Outcome? {
        val run = transaction { db ->
            db.queryOne("select run_id from download where episode_id = ? and state = 'transferring'", episodeId) { it.string("run_id") }
        } ?: return null
        running.update { it + episodeId }
        try {
            return transfer.run(episodeId, run) { progress -> progressState.update { it + (episodeId to progress) } }
        } finally {
            running.update { it - episodeId }
            progressState.update { it - episodeId }
            wake()
        }
    }

    /**
     * Logout's step 2 (spec 5.7): cancels every transfer and waits up to [timeoutMs] for them to
     * stop, then runs [block] with passes held off, so no transfer starts or requeues a row of the
     * store being wiped. A transfer that outlives the wait finds its row gone at its next write and
     * deletes its own files.
     */
    suspend fun <T> stopped(timeoutMs: Long, block: suspend () -> T): T = passLock.withLock {
        jobs.cancelAll()
        withTimeoutOrNull(timeoutMs) {
            while (live().isNotEmpty()) delay(100)
        }
        progressState.value = emptyMap()
        block()
    }
}
