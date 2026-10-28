package ltd.kilian.pod.sync

import android.util.Log
import androidx.sqlite.db.SupportSQLiteDatabase
import ltd.kilian.pod.core.MAX_MUTATIONS
import ltd.kilian.pod.core.Mutation
import ltd.kilian.pod.core.PodcastCursor
import ltd.kilian.pod.core.ScopeCursors
import ltd.kilian.pod.core.SyncRequest
import ltd.kilian.pod.core.SyncResponse
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull

// ==============================================================================================
// The store steps of one sync round (spec 5.3)
// ==============================================================================================
//
// A round is three steps: one transaction selects the outbox batch, marks it in flight and reads
// the cursors ([prepare]); the request goes out; one transaction applies the response and
// decides whether the loop goes on ([finishRound]). The HTTP step between them is the engine's.
// Ports of pod-desktop's `prepare` and `finish_round` in `sync/engine.rs`.

/** Changes per response: the server's default, and enough for a backfill to show progress. */
const val PAGE_LIMIT = 500

/** A batch that fails with `400` this many times is dropped (spec 5.3). */
const val MAX_ATTEMPTS = 5

/** One round's request and what the engine needs to send and apply it. */
data class Prepared(
    val serverUrl: String,
    val token: String,
    val userId: String,
    val batch: List<String>,
    val request: SyncRequest,
    /** The show whose episodes are loading, for "Loading episodes for <show>" (spec 5.6). */
    val backfill: String?,
) {
    override fun toString() = "Prepared(serverUrl=$serverUrl, token=<redacted>, userId=$userId, batch=$batch, backfill=$backfill)"
}

enum class Round {
    /** Something is left to push or pull. */
    More,
    Done,
    NoAccount,
    AccountChanged,

    /** A response moved no cursor and acknowledged nothing; asking again would loop forever. */
    Stalled,
}

/** Reads the account, selects and marks the batch, and lists the cursors. Null without an account. */
fun prepare(db: SupportSQLiteDatabase, pullOnly: Boolean, inFlight: InFlight): Prepared? {
    val account = db.queryOne("select server_url, token, user_id from account where id = 1") {
        Triple(it.string("server_url"), it.string("token"), it.string("user_id"))
    } ?: return null

    // Oldest first, so the server applies mutations in the order the user made them. The id
    // breaks ties within a millisecond.
    val rows = if (pullOnly) emptyList() else db.queryList(
        "select mutation_id, payload from outbox order by created_at, mutation_id limit ?", MAX_MUTATIONS,
    ) { it.string("mutation_id") to it.string("payload") }
    val batch = rows.map { it.first }
    inFlight.set(batch)

    // The user scope first, then podcast scopes that were complete, so fresh deltas don't wait
    // behind a backfill, then the incomplete ones.
    var user = 0L
    val podcasts = mutableListOf<PodcastCursor>()
    var backfill: String? = null
    val scopes = db.queryList("select scope, version, complete from sync_scope order by complete desc, scope") {
        Triple(it.string("scope"), it.long("version"), it.flag("complete"))
    }
    for ((scope, version, complete) in scopes) {
        if (scope == "user") {
            user = version
            continue
        }
        val id = scope.removePrefix("podcast:")
        if (!complete && backfill == null) {
            backfill = db.queryOne("select coalesce(title, local_title) as name from podcast where id = ?", id) { it.stringOrNull("name") }
        }
        podcasts += PodcastCursor(id, version)
    }

    return Prepared(
        serverUrl = account.first,
        token = account.second,
        userId = account.third,
        batch = batch,
        request = SyncRequest(
            mutations = rows.map { WireJson.decodeFromString(Mutation.serializer(), it.second) },
            scopes = ScopeCursors(user, podcasts),
            limit = PAGE_LIMIT,
        ),
        backfill = backfill,
    )
}

/** Applies the response to the request [prepare] made for [userId], clears the in-flight set, and decides whether the loop goes on. */
fun finishRound(
    db: SupportSQLiteDatabase,
    inFlight: InFlight,
    userId: String,
    response: SyncResponse,
    pullOnly: Boolean,
    now: Long,
    inserted: MutableSet<String> = mutableSetOf(),
): Round {
    val before = progress(db)
    val applied = try {
        applyResponse(db, userId, response, now, inserted)
    } finally {
        inFlight.clear()
    }
    if (applied == Applied.AccountChanged) return Round.AccountChanged
    if (loopIsDone(db, pullOnly)) return Round.Done
    // A matching server always moves a cursor or acknowledges a mutation.
    if (progress(db) == before) return Round.Stalled
    return Round.More
}

/** Counts a `400` against every row of the batch and moves rows that reached [MAX_ATTEMPTS] to the rejected log. */
fun countAttempt(db: SupportSQLiteDatabase, batch: List<String>, now: Long) {
    for (id in batch) {
        db.exec("update outbox set attempts = attempts + 1 where mutation_id = ?", id)
        val exhausted = db.queryOne("select payload from outbox where mutation_id = ? and attempts >= ?", id, MAX_ATTEMPTS) { it.string("payload") }
            ?: continue
        Log.w("PodSync", "dropping mutation $id, which the server keeps refusing")
        db.exec("delete from outbox where mutation_id = ?", id)
        logRejection(db, id, exhausted, "bad_request", now)
    }
}

/** Every cursor and the outbox size, to tell whether a response moved anything. */
private fun progress(db: SupportSQLiteDatabase): Pair<List<Triple<String, Long, Boolean>>, Long> =
    db.queryList("select scope, version, complete from sync_scope order by scope") {
        Triple(it.string("scope"), it.long("version"), it.flag("complete"))
    } to db.queryOne("select count(*) as n from outbox") { it.long("n") }!!

/**
 * Whether the loop can stop: nothing left to push (or pushing is off for this loop) and every
 * scope complete. A scope new in this response starts incomplete, so it gets its first page.
 */
private fun loopIsDone(db: SupportSQLiteDatabase, pullOnly: Boolean): Boolean {
    val incomplete = db.queryOne("select count(*) as n from sync_scope where complete = 0") { it.long("n") }!!
    val queued = if (pullOnly) 0 else db.queryOne("select count(*) as n from outbox") { it.long("n") }!!
    return incomplete + queued == 0L
}
