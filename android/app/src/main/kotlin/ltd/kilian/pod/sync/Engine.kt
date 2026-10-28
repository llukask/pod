package ltd.kilian.pod.sync

import android.util.Log
import androidx.sqlite.db.SupportSQLiteDatabase
import java.io.IOException
import kotlin.math.max
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.SerializationException
import ltd.kilian.pod.core.ErrorBody
import ltd.kilian.pod.core.SyncRequest
import ltd.kilian.pod.core.SyncResponse
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.JSON
import ltd.kilian.pod.net.NetworkState
import ltd.kilian.pod.net.fetch
import ltd.kilian.pod.store.PodDatabase
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

// ==============================================================================================
// The sync loop (spec 5.3)
// ==============================================================================================
//
// Push the outbox and pull every scope, one page per `POST /v1/sync`, until the outbox is empty
// and every scope complete. One engine runs per process. [SyncEngine.sync] holds its mutex for
// a whole loop, so two loops never interleave: the foreground driver and the WorkManager job
// share it (spec 5.6), and logout takes it to keep a response from landing after it (5.7).
//
// Each round is three steps: one transaction selects the outbox batch, marks it in flight and
// reads the cursors ([prepare]); the request goes out; one transaction applies the response and
// clears the in-flight set ([finishRound]). The engine publishes what it is doing as an
// [EngineState], which the sync status reads. A port of pod-desktop's `sync/engine.rs`.

/** The first retry after a failed request waits this long; each further failure doubles it up to [MAX_BACKOFF_MS]. */
const val FIRST_BACKOFF_MS = 5_000L
const val MAX_BACKOFF_MS = 15 * 60_000L

/** How often the driver syncs while nothing else asks it to (spec 5.6, foreground timer). */
const val TIMER_INTERVAL_MS = 5 * 60_000L

/** What the engine is doing, for the sync status (spec 5.6). */
sealed interface EngineState {
    /** Not syncing. [lastSynced] is the wall-clock time a loop last finished, null before the first one in this process. */
    data class Idle(val lastSynced: Long?) : EngineState

    /** A loop is running. [backfill] names the show whose episodes are loading. */
    data class Running(val backfill: String?) : EngineState

    /** The OS reports no connection. */
    data object Offline : EngineState

    /** A request failed with a network error, `5xx` or `429`; the next try is at [retryAt] ([monotonicMs]). */
    data class BackingOff(val retryAt: Long) : EngineState

    /** The server answered `401`. Local use goes on; signing in again resumes sync (spec 5.7). */
    data object SignedOut : EngineState
}

/** Why a request failed as a whole. The error code set is open, so this goes by HTTP status alone (spec 4). */
sealed interface RequestFailure {
    data object SignedOut : RequestFailure

    /** `400`: the envelope or a mutation id is broken, a client bug. */
    data object BadRequest : RequestFailure

    /** A network error, `5xx`, `429`, or a body that isn't a sync response. */
    data class Retry(val retryAfterMs: Long?) : RequestFailure

    /** Any other `4xx`, which retrying won't fix. */
    data class Refused(val status: Int) : RequestFailure
}

class SyncEngine(
    private val database: PodDatabase,
    val handle: SyncHandle,
    /** The client sync and the other requests to the account's server share. */
    val http: OkHttpClient,
    /** What the OS reports about the network; the download reconciler reads it too. */
    val network: StateFlow<NetworkState>,
    private val clock: () -> Long = ::monotonicMs,
) {
    /** The process-wide sync mutex. Logout holds it through a [SyncPause]. */
    private val run = Mutex()

    // What loops carry from one to the next, guarded by [run].
    /** Requests that failed in a row with a retryable error. */
    private var failures = 0
    private var lastSynced: Long? = null

    /** How many [pause] calls wait for the mutex. A loop holding it stops as soon as this is non-zero, dropping its request. */
    private val pausing = MutableStateFlow(0)

    private val published = MutableStateFlow<EngineState>(EngineState.Idle(null))
    val state: StateFlow<EngineState> = published.asStateFlow()

    private val ended = MutableSharedFlow<Unit>(extraBufferCapacity = 1, onBufferOverflow = BufferOverflow.DROP_OLDEST)

    /** Emits each time a sync loop finishes, for the download reconciler (spec 7.4). */
    val loopEnded: SharedFlow<Unit> = ended

    /** Holds the sync mutex. No loop runs until it is closed. */
    class SyncPause internal constructor(private val mutex: Mutex) : AutoCloseable {
        override fun close() = mutex.unlock()
    }

    /**
     * Cancels the running loop, dropping its request, and holds the sync mutex until the
     * returned pause closes, so no response is applied in between (spec 5.7). Also forgets what
     * the loops carried over: the backoff and the last sync time belong to the account logout
     * clears.
     */
    suspend fun pause(): SyncPause {
        pausing.update { it + 1 }
        try {
            run.lock()
        } finally {
            pausing.update { it - 1 }
        }
        failures = 0
        lastSynced = null
        published.value = EngineState.Idle(null)
        return SyncPause(run)
    }

    // ==========================================================================================
    // The loop
    // ==========================================================================================

    /**
     * Runs the sync loop until there is nothing left to push or pull, or a request fails, and
     * returns the state it ended in. Waits for a loop already running to end first. Throws when
     * the local store fails; request failures end in a state.
     */
    suspend fun sync(): EngineState = run.withLock {
        val result = coroutineScope {
            val loop = async { loopUntilQuiet() }
            val watcher = launch {
                pausing.first { it > 0 }
                loop.cancel()
            }
            try {
                loop.await()
            } catch (e: CancellationException) {
                // A pause cancelled the loop, unless the caller itself is being cancelled. A store
                // write the loop already started still commits; the account check in the apply
                // step (or logout's delete after it) covers that.
                coroutineContext.ensureActive()
                null
            } finally {
                watcher.cancel()
                handle.inFlight.clear()
            }
        }
        // After a pause, the pause publishes the state and its holder decides what happens next.
        if (result == null) return@withLock EngineState.Idle(lastSynced)
        published.value = result
        ended.tryEmit(Unit)
        result
    }

    private suspend fun loopUntilQuiet(): EngineState {
        while (true) {
            handle.takeWanted()
            if (!network.value.connected) return EngineState.Offline
            val state = runLoop()
            // A local write during the loop asked for a sync. Its row may have missed the last
            // request, so run once more.
            if (state is EngineState.Idle && handle.wanted.value != null) continue
            return state
        }
    }

    private suspend fun runLoop(): EngineState {
        var pullOnly = false
        while (true) {
            when (val outcome = round(pullOnly)) {
                is RoundOutcome.Applied -> when (outcome.round) {
                    Round.More -> failures = 0
                    Round.Done -> {
                        failures = 0
                        lastSynced = System.currentTimeMillis()
                        return EngineState.Idle(lastSynced)
                    }
                    Round.NoAccount, Round.AccountChanged -> return EngineState.Idle(lastSynced)
                    Round.Stalled -> {
                        Log.e(TAG, "a sync response moved no cursor; stopping the loop")
                        return EngineState.Idle(lastSynced)
                    }
                }
                is RoundOutcome.Failed -> when (val failure = outcome.failure) {
                    RequestFailure.SignedOut -> return EngineState.SignedOut
                    // The batch may hold a mutation the server can't even key a result by. Pull
                    // without it for the rest of the loop; the next loop tries the batch again.
                    RequestFailure.BadRequest -> if (!pullOnly) {
                        pullOnly = true
                    } else {
                        Log.e(TAG, "the server refused a pull-only sync request")
                        return EngineState.Idle(lastSynced)
                    }
                    is RequestFailure.Retry -> {
                        if (!network.value.connected) return EngineState.Offline
                        failures += 1
                        val backoff = minOf(FIRST_BACKOFF_MS shl minOf(failures - 1, 16), MAX_BACKOFF_MS)
                        return EngineState.BackingOff(clock() + max(backoff, failure.retryAfterMs ?: 0))
                    }
                    is RequestFailure.Refused -> {
                        // TODO: surface a refused request in the sync status. Spec 5.6 has no
                        // wording for it yet, and with a matching server it can't happen.
                        Log.e(TAG, "the server refused the sync request with ${failure.status}")
                        return EngineState.Idle(lastSynced)
                    }
                }
            }
        }
    }

    private sealed interface RoundOutcome {
        data class Applied(val round: Round) : RoundOutcome

        data class Failed(val failure: RequestFailure) : RoundOutcome
    }

    private suspend fun round(pullOnly: Boolean): RoundOutcome {
        val prepared = transaction { prepare(it, pullOnly, handle.inFlight) }
            ?: return RoundOutcome.Applied(Round.NoAccount)
        published.value = EngineState.Running(prepared.backfill)

        val response = when (val posted = post(prepared)) {
            is Posted.Ok -> posted.response
            is Posted.Failed -> {
                handle.inFlight.clear()
                if (posted.failure == RequestFailure.BadRequest && prepared.batch.isNotEmpty()) {
                    transaction { countAttempt(it, prepared.batch, System.currentTimeMillis()) }
                }
                return RoundOutcome.Failed(posted.failure)
            }
        }
        return RoundOutcome.Applied(
            transaction { finishRound(it, handle.inFlight, prepared.userId, response, pullOnly, System.currentTimeMillis()) },
        )
    }

    private sealed interface Posted {
        data class Ok(val response: SyncResponse) : Posted

        data class Failed(val failure: RequestFailure) : Posted
    }

    private suspend fun post(prepared: Prepared): Posted {
        val request = Request.Builder()
            .url(prepared.serverUrl.trimEnd('/') + "/v1/sync")
            .header("Authorization", "Bearer ${prepared.token}")
            .post(WireJson.encodeToString(SyncRequest.serializer(), prepared.request).toRequestBody(JSON))
            .build()
        val fetched = try {
            http.fetch(request)
        } catch (e: IOException) {
            Log.i(TAG, "the sync request failed: $e")
            return Posted.Failed(RequestFailure.Retry(null))
        }
        if (fetched.isSuccessful) {
            return try {
                Posted.Ok(WireJson.decodeFromString(SyncResponse.serializer(), fetched.body))
            } catch (e: SerializationException) {
                Log.w(TAG, "the sync response doesn't parse: $e")
                Posted.Failed(RequestFailure.Retry(null))
            } catch (e: IllegalArgumentException) {
                Log.w(TAG, "the sync response doesn't parse: $e")
                Posted.Failed(RequestFailure.Retry(null))
            }
        }
        val code = runCatching { WireJson.decodeFromString(ErrorBody.serializer(), fetched.body).error.code }.getOrNull()
        Log.i(TAG, "the server failed the sync request: ${fetched.status} $code")
        return Posted.Failed(
            when {
                fetched.status == 401 -> RequestFailure.SignedOut
                fetched.status == 400 -> RequestFailure.BadRequest
                fetched.status == 429 -> RequestFailure.Retry(fetched.headers["Retry-After"]?.trim()?.toLongOrNull()?.times(1000))
                fetched.status >= 500 -> RequestFailure.Retry(null)
                else -> RequestFailure.Refused(fetched.status)
            },
        )
    }

    private suspend fun <T> transaction(block: (SupportSQLiteDatabase) -> T): T = withContext(Dispatchers.IO) {
        database.runInTransaction<T> { block(database.openHelper.writableDatabase) }
    }

    // ==========================================================================================
    // Triggers (spec 5.6)
    // ==========================================================================================

    private sealed interface Trigger {
        data object Wanted : Trigger

        data class Network(val state: NetworkState) : Trigger
    }

    /**
     * Syncs at start, then whenever a local write's deadline comes due, the foreground timer
     * fires, connectivity goes or returns, or a backoff ends. While a subscription waits for the
     * server's first fetch of its feed, the timer runs on [FIRST_FETCH_RESYNCS_MS] instead. Runs
     * until cancelled.
     *
     * During a backoff only its end or a connectivity change wake the engine, so writes don't
     * hammer a failing server. While signed out only a change to the account row does: a
     * sign-in, or logout.
     *
     * The driver runs for the life of the process. Android freezes a cached background process,
     * so the timer costs nothing while the app is out of sight; a playing app is a foreground
     * service and keeps syncing its position saves.
     */
    suspend fun run(): Unit = coroutineScope {
        // Counted from before the sync, so a sign-in that lands while a request is being refused
        // still wakes the engine.
        val accountChanges = MutableStateFlow(0)
        launch {
            database.invalidationTracker.createFlow("account", emitInitialState = false).collect { accountChanges.update { it + 1 } }
        }
        val firstFetch = FirstFetchResyncs()
        while (true) {
            val accountSeen = accountChanges.value
            val state = try {
                sync()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                Log.e(TAG, "sync failed locally", e)
                EngineState.Idle(null)
            }
            if (state == EngineState.SignedOut) {
                accountChanges.first { it != accountSeen }
                continue
            }
            val resync = if (state is EngineState.Idle) firstFetch.next(transaction(::awaitingFirstFetch)) else null
            waitForTrigger(state, clock() + (resync ?: TIMER_INTERVAL_MS))
        }
    }

    private suspend fun waitForTrigger(state: EngineState, timer: Long) {
        while (true) {
            val wanted = handle.wanted.value
            val seen = network.value
            val wake = if (state is EngineState.BackingOff) state.retryAt else minOf(timer, wanted ?: timer)
            val trigger = withTimeoutOrNull(max(0, wake - clock())) {
                merge(
                    handle.wanted.filter { it != wanted }.map { Trigger.Wanted },
                    network.filter { it != seen }.map { Trigger.Network(it) },
                ).first()
            } ?: return
            if (trigger is Trigger.Network) {
                val connected = trigger.state.connected
                // A lost connection shows in the status at once; the loop ends offline without a
                // request.
                val lost = !connected && state != EngineState.Offline
                val regained = connected && (state == EngineState.Offline || state is EngineState.BackingOff)
                if (lost || regained) return
            }
        }
    }

    private companion object {
        const val TAG = "PodSync"
    }
}

/**
 * The waits between syncs while a subscription waits for the server's first fetch of its feed,
 * which the poller starts right after the subscribe lands. After the last one the timer takes
 * over.
 */
val FIRST_FETCH_RESYNCS_MS = listOf(2_000L, 5_000L, 15_000L, 30_000L)

/** Walks [FIRST_FETCH_RESYNCS_MS] while shows wait for their first fetch. */
class FirstFetchResyncs {
    /** The shows waiting at the last check. */
    private var waiting = emptySet<String>()

    /** How many of the waits the shows waiting now have used. */
    private var used = 0

    /**
     * The wait until the next sync, given the shows waiting after the one that just ended. Null
     * leaves it to the timer: nothing is waiting, or the schedule ran out. A show that wasn't
     * waiting before starts it over.
     */
    fun next(waiting: Set<String>): Long? {
        if (!this.waiting.containsAll(waiting)) used = 0
        this.waiting = waiting
        if (waiting.isEmpty()) {
            used = 0
            return null
        }
        val wait = FIRST_FETCH_RESYNCS_MS.getOrNull(used)
        used = minOf(used + 1, FIRST_FETCH_RESYNCS_MS.size)
        return wait
    }
}
