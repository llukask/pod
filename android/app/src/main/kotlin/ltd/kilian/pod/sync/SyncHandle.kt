package ltd.kilian.pod.sync

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.flow.update

// Local writes and the sync loop meet in two places:
//
// - The in-flight set: the outbox rows that are part of the request being sent. Coalescing must
//   leave those alone, because the response deletes rows by `mutation_id` and a newer coalesced
//   row has to survive it.
// - The sync request: when the engine should next run. A write asks for a sync by a deadline,
//   and the earliest outstanding deadline stands.

/** A monotonic clock in milliseconds, for deadlines. Not the wall clock the stamps use. */
fun monotonicMs(): Long = System.nanoTime() / 1_000_000

/**
 * The mutation ids of the request being sent (spec 5.2). A local write must not coalesce away a
 * row in this set, because its result is still to come and deletes it by id.
 *
 * Local writes check the set inside their transaction while holding its lock, so it can't
 * change between the coalescing check and the commit.
 */
class InFlight {
    private val ids = mutableSetOf<String>()

    fun set(batch: Collection<String>) = synchronized(this) {
        ids.clear()
        ids.addAll(batch)
    }

    fun clear() = set(emptyList())

    fun <T> withLock(block: (Set<String>) -> T): T = synchronized(this) { block(ids.toSet()) }
}

/** What local writes share with the sync engine. One per process. */
class SyncHandle(private val clock: () -> Long = ::monotonicMs) {
    val inFlight = InFlight()

    private val deadline = MutableStateFlow<Long?>(null)

    /** The [monotonicMs] deadline by which a sync should start, null when nothing asked for one. */
    val wanted: StateFlow<Long?> = deadline.asStateFlow()

    /**
     * Asks for a sync to start within [delayMs]. An earlier outstanding deadline stands, so a
     * burst of writes shares one request and a 2 s write isn't held back by a pending 30 s
     * position write. A write made while a request is in flight leaves a deadline behind, which
     * tells the loop to run once more (spec 5.3).
     */
    fun requestSync(delayMs: Long) {
        val requested = clock() + delayMs
        deadline.update { earlier -> if (earlier != null && earlier <= requested) earlier else requested }
    }

    /** Clears the outstanding request and returns its deadline. The engine calls this as a sync starts, so writes made during it ask again. */
    fun takeWanted(): Long? = deadline.getAndUpdate { null }
}
