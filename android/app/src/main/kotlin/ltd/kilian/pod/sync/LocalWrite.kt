package ltd.kilian.pod.sync

import androidx.sqlite.db.SupportSQLiteDatabase
import java.security.SecureRandom
import java.util.UUID
import kotlinx.serialization.json.JsonElement
import ltd.kilian.pod.core.Mutation
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string

// ==============================================================================================
// Local writes (spec 5.2)
// ==============================================================================================
//
// Every user action is one local transaction that overwrites the local row, queues a coalesced
// outbox mutation and asks the sync engine to run. Local writes never go through `merge`; they
// stamp the field with `at = max(now, previous at + 1 ms)`, so the user's latest action always
// wins locally, even after a pull brought in a value stamped by a device whose clock runs fast.
//
// The device id is the signed-in session's, from the `account` row. Without an account there is
// no user state to write (logout clears it, spec 5.7). A `401` keeps the account row, so writes
// keep working while signed out. A port of pod-desktop's `sync/local_write.rs`.

/** How long a position save during playback may wait for a sync, so playing isn't a request every 5 s. */
const val PLAYBACK_SYNC_DELAY_MS = 30_000L

/** How long every other write waits, so a burst of actions shares a request. */
const val WRITE_SYNC_DELAY_MS = 2_000L

/** A user action on synced state, without the stamp the write adds. */
sealed interface LocalChange {
    /** One field of an episode's state. [podcastId] fills a new `episode_state` row. */
    data class Episode(val episodeId: String, val podcastId: String, val change: EpisodeChange) : LocalChange

    /** Subscribe or unsubscribe: the `subscription` row and its mutation only. */
    data class Subscription(val podcastId: String, val feedUrl: String, val subscribed: Boolean) : LocalChange

    data class Setting(val key: String, val value: JsonElement) : LocalChange

    /** How soon the sync engine should send this change. */
    val syncDelayMs: Long
        get() = if (this is Episode && change is EpisodeChange.Position && change.whilePlaying) PLAYBACK_SYNC_DELAY_MS else WRITE_SYNC_DELAY_MS
}

/**
 * The `episode_state` field an action writes: [valueColumn] and the `<field>_at` / `<field>_dev`
 * stamp columns.
 */
sealed class EpisodeChange(val field: String, val valueColumn: String, val value: Long) {
    /** [whilePlaying] marks the periodic saves during playback; pause, stop and seek aren't. */
    data class Position(val positionMs: Long, val whilePlaying: Boolean) : EpisodeChange("position", "position_ms", positionMs) {
        init {
            require(positionMs >= 0) { "a position can't be negative, got $positionMs" }
        }
    }

    data class Played(val played: Boolean) : EpisodeChange("played", "played", if (played) 1 else 0)

    data class Archived(val archived: Boolean) : EpisodeChange("archived", "archived", if (archived) 1 else 0)
}

/** The stamp for a local write at [now] to a field last stamped [previous]: `max(now, previous + 1 ms)`. */
fun localStampAt(now: Long, previous: Long?): Long = if (previous == null) now else maxOf(now, previous + 1)

private val random = SecureRandom()

/** A fresh mutation id. UUIDv7, so ids sort by creation time like the server's. */
fun newMutationId(now: Long = System.currentTimeMillis()): String {
    val high = (now shl 16) or (0x7000L) or (random.nextInt(0x1000).toLong())
    val low = (random.nextLong() and 0x3fffffffffffffffL) or Long.MIN_VALUE
    return UUID(high, low).toString()
}

/**
 * Makes [change] locally inside the caller's transaction and queues its mutation. [inFlight] is
 * the set the caller holds the lock of. Returns the queued mutation.
 */
fun applyLocalChange(db: SupportSQLiteDatabase, change: LocalChange, id: String, inFlight: Set<String>, now: Long): Mutation {
    val dev = db.queryOne("select device_id from account where id = 1") { it.string("device_id") }
        ?: error("no account is signed in")
    val mutation = when (change) {
        is LocalChange.Episode -> {
            val field = change.change.field
            val at = localStampAt(now, previousAt(db, "select ${field}_at as at from episode_state where episode_id = ?", change.episodeId))
            db.exec(
                """insert into episode_state (episode_id, podcast_id, ${change.change.valueColumn}, ${field}_at, ${field}_dev)
                   values (?, ?, ?, ?, ?)
                   on conflict (episode_id) do update set
                       ${change.change.valueColumn} = excluded.${change.change.valueColumn},
                       ${field}_at = excluded.${field}_at, ${field}_dev = excluded.${field}_dev""",
                change.episodeId, change.podcastId, change.change.value, at, dev,
            )
            when (val c = change.change) {
                is EpisodeChange.Position -> Mutation.EpisodePosition(id, at, dev, change.episodeId, c.positionMs)
                is EpisodeChange.Played -> Mutation.EpisodePlayed(id, at, dev, change.episodeId, c.played)
                is EpisodeChange.Archived -> Mutation.EpisodeArchived(id, at, dev, change.episodeId, c.archived)
            }
        }
        is LocalChange.Subscription -> {
            val at = localStampAt(now, previousAt(db, "select subscribed_at as at from subscription where podcast_id = ?", change.podcastId))
            // `replaced_by` is the server's to set (spec 5.5), so a local write leaves it alone.
            db.exec(
                """insert into subscription (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
                   values (?, ?, ?, ?, ?)
                   on conflict (podcast_id) do update set
                       feed_url = excluded.feed_url, subscribed = excluded.subscribed,
                       subscribed_at = excluded.subscribed_at, subscribed_dev = excluded.subscribed_dev""",
                change.podcastId, change.feedUrl, change.subscribed, at, dev,
            )
            Mutation.SubscriptionSet(id, at, dev, change.podcastId, change.feedUrl, change.subscribed)
        }
        is LocalChange.Setting -> {
            val at = localStampAt(now, previousAt(db, "select value_at as at from setting where key = ?", change.key))
            db.exec(
                """insert into setting (key, value, value_at, value_dev) values (?, ?, ?, ?)
                   on conflict (key) do update set
                       value = excluded.value, value_at = excluded.value_at, value_dev = excluded.value_dev""",
                change.key, WireJson.encodeToString(JsonElement.serializer(), change.value), at, dev,
            )
            Mutation.SettingSet(id, at, dev, change.key, change.value)
        }
    }
    enqueue(db, mutation, inFlight, now)
    return mutation
}

private fun previousAt(db: SupportSQLiteDatabase, query: String, key: String): Long? =
    db.queryOne(query, key) { it.longOrNull("at") }

/** Deletes the queued, not-in-flight outbox rows with the mutation's coalesce key and inserts it. */
private fun enqueue(db: SupportSQLiteDatabase, mutation: Mutation, inFlight: Set<String>, now: Long) {
    val queued = db.queryList("select mutation_id from outbox where coalesce_key = ?", mutation.coalesceKey) { it.string("mutation_id") }
    queued.filterNot { it in inFlight }.forEach { db.exec("delete from outbox where mutation_id = ?", it) }
    // `created_at` is the wall clock, not the mutation's `at`, so the outbox keeps the order the
    // user acted in even when a field's stamp was pushed past `now` by a future-stamped pull.
    db.exec(
        "insert into outbox (mutation_id, coalesce_key, payload, created_at) values (?, ?, ?, ?)",
        mutation.id, mutation.coalesceKey, WireJson.encodeToString(Mutation.serializer(), mutation), now,
    )
}
