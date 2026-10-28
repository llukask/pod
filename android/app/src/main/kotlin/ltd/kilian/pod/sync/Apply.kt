package ltd.kilian.pod.sync

import android.util.Log
import androidx.sqlite.db.SupportSQLiteDatabase
import ltd.kilian.pod.core.Episode
import ltd.kilian.pod.core.EpisodeTombstone
import ltd.kilian.pod.core.FeedError
import ltd.kilian.pod.core.Mutation
import ltd.kilian.pod.core.MutationStatus
import ltd.kilian.pod.core.PodcastScope
import ltd.kilian.pod.core.PodcastStatus
import ltd.kilian.pod.core.ScopeChanges
import ltd.kilian.pod.core.SyncResponse
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.store.EpisodeStateRow
import ltd.kilian.pod.store.SettingRow
import ltd.kilian.pod.store.SubscriptionRow
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull

// ==============================================================================================
// Applying one `POST /v1/sync` response (spec 5.3), inside the caller's transaction
// ==============================================================================================
//
// [applyResponse] needs only the transaction, the user the request was made for and the
// response: the mutations the request carried are still in the outbox (rows in flight can't be
// coalesced away), so it reads them from there by the result ids. That keeps it drivable by the
// conformance fixtures without HTTP. A port of pod-desktop's `sync/apply.rs`.
//
// Every write is an idempotent upsert or delete by id, and the cursors move in the same
// transaction as the rows, so a crash re-applies at most one page and that changes nothing.

private const val TAG = "PodSync"

/** How many rejected mutations the local log keeps (spec 5.6). */
private const val REJECTED_LOG_SIZE = 50

enum class Applied {
    Applied,

    /**
     * The account row is gone or belongs to another user, so the response answers a session
     * that no longer exists (logout or an account switch during the request, spec 5.7). Nothing
     * was written.
     */
    AccountChanged,
}

/**
 * Applies one sync response in the caller's transaction. The ids of episodes this response
 * inserted (not updated) go into [inserted], for the new-episode notification (spec 7.6).
 */
fun applyResponse(
    db: SupportSQLiteDatabase,
    userId: String,
    response: SyncResponse,
    now: Long,
    inserted: MutableSet<String> = mutableSetOf(),
): Applied {
    val account = db.queryOne("select user_id from account where id = 1") { it.string("user_id") }
    if (account != userId) return Applied.AccountChanged

    applyResults(db, response, now)
    response.scopes?.let { scopes ->
        mergeUserScope(db, scopes)
        // After the merge, so Y's own row from this page is in place for the carry, and before
        // the reconcile, which then gives Y its scope.
        for (remote in scopes.user.subscriptions) {
            remote.replacedBy?.let { canonical -> followReplacement(db, remote.podcastId, canonical) }
        }
        reconcileScopes(db)
        scopes.podcasts.forEach { applyPodcastScope(db, it, inserted) }
    }
    return Applied.Applied
}

// ----------------------------------------------------------------------------------------------
// Mutation results
// ----------------------------------------------------------------------------------------------

/**
 * Deletes acknowledged outbox rows by `mutation_id`, never by key, so a newer coalesced row
 * written during the request survives. Copies a clamped `at` into the field the mutation wrote
 * while that field still carries the sent stamp, for every status (spec 5.3, 4.4).
 */
private fun applyResults(db: SupportSQLiteDatabase, response: SyncResponse, now: Long) {
    for (result in response.results) {
        // A result for a row that is gone was applied before: the same page again after a crash.
        val payload = db.queryOne("select payload from outbox where mutation_id = ?", result.id) { it.string("payload") }
            ?: continue
        db.exec("delete from outbox where mutation_id = ?", result.id)
        val sent = WireJson.decodeFromString(Mutation.serializer(), payload)

        if (result.status == MutationStatus.Rejected) {
            // The code set is open (spec 4.3): an unknown code is logged and dropped like any other.
            val reason = result.code ?: "unknown"
            Log.w(TAG, "the server rejected mutation ${result.id}: $reason")
            if (reason == "invalid_feed_url" && sent is Mutation.SubscriptionSet) forgetRejectedSubscribe(db, sent)
            logRejection(db, result.id, payload, reason, now)
        }
        val storedAt = result.at
        if (storedAt != null && storedAt != sent.at) rewriteStamp(db, sent, storedAt)
    }
}

/**
 * Sets the `*_at` of the field [sent] wrote to the server's [storedAt], if the field still
 * carries the stamp [sent] went out with. Compared with the mutation's own `dev`, which a
 * re-login doesn't change.
 */
private fun rewriteStamp(db: SupportSQLiteDatabase, sent: Mutation, storedAt: Long) {
    val (table, keyColumn, key, field) = when (sent) {
        is Mutation.SubscriptionSet -> listOf("subscription", "podcast_id", sent.podcastId, "subscribed")
        is Mutation.EpisodePosition -> listOf("episode_state", "episode_id", sent.episodeId, "position")
        is Mutation.EpisodePlayed -> listOf("episode_state", "episode_id", sent.episodeId, "played")
        is Mutation.EpisodeArchived -> listOf("episode_state", "episode_id", sent.episodeId, "archived")
        is Mutation.SettingSet -> listOf("setting", "key", sent.key, "value")
    }
    db.exec(
        "update $table set ${field}_at = ? where $keyColumn = ? and ${field}_at = ? and ${field}_dev = ?",
        storedAt, key, sent.at, sent.dev,
    )
}

/** Records a mutation the server won't take in the log Settings shows, keeping the newest 50. */
fun logRejection(db: SupportSQLiteDatabase, mutationId: String, payload: String, reason: String, now: Long) {
    db.exec(
        "insert or replace into rejected_mutation (mutation_id, payload, reason, rejected_at) values (?, ?, ?, ?)",
        mutationId, payload, reason, now,
    )
    db.exec(
        """delete from rejected_mutation where mutation_id not in (
               select mutation_id from rejected_mutation order by rejected_at desc, mutation_id desc limit ?)""",
        REJECTED_LOG_SIZE,
    )
}

// ----------------------------------------------------------------------------------------------
// User scope
// ----------------------------------------------------------------------------------------------

private fun mergeUserScope(db: SupportSQLiteDatabase, scopes: ScopeChanges) {
    val user = scopes.user
    for (remote in user.subscriptions) writeSubscription(db, merge(loadSubscription(db, remote.podcastId), remote))
    for (remote in user.episodeStates) {
        val row = merge(loadEpisodeState(db, remote.episodeId), remote)
        db.exec(
            """insert or replace into episode_state
                   (episode_id, podcast_id, position_ms, position_at, position_dev,
                    played, played_at, played_dev, archived, archived_at, archived_dev)
               values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            row.episodeId, row.podcastId, row.positionMs, row.positionAt, row.positionDev,
            row.played, row.playedAt, row.playedDev, row.archived, row.archivedAt, row.archivedDev,
        )
    }
    for (remote in user.settings) {
        val row = merge(loadSetting(db, remote.key), remote)
        db.exec(
            "insert or replace into setting (key, value, value_at, value_dev) values (?, ?, ?, ?)",
            row.key, row.value, row.valueAt, row.valueDev,
        )
    }
    setScope(db, "user", user.version, user.complete)
}

internal fun writeSubscription(db: SupportSQLiteDatabase, row: SubscriptionRow) {
    db.exec(
        """insert or replace into subscription
               (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev, replaced_by)
           values (?, ?, ?, ?, ?, ?)""",
        row.podcastId, row.feedUrl, row.subscribed, row.subscribedAt, row.subscribedDev, row.replacedBy,
    )
}

internal fun loadSubscription(db: SupportSQLiteDatabase, podcastId: String): SubscriptionRow? =
    db.queryOne("select * from subscription where podcast_id = ?", podcastId) {
        SubscriptionRow(
            podcastId = it.string("podcast_id"),
            feedUrl = it.string("feed_url"),
            subscribed = it.flag("subscribed"),
            subscribedAt = it.long("subscribed_at"),
            subscribedDev = it.string("subscribed_dev"),
            replacedBy = it.stringOrNull("replaced_by"),
        )
    }

private fun loadEpisodeState(db: SupportSQLiteDatabase, episodeId: String): EpisodeStateRow? =
    db.queryOne("select * from episode_state where episode_id = ?", episodeId) {
        EpisodeStateRow(
            episodeId = it.string("episode_id"),
            podcastId = it.string("podcast_id"),
            positionMs = it.long("position_ms"),
            positionAt = it.longOrNull("position_at"),
            positionDev = it.stringOrNull("position_dev"),
            played = it.flag("played"),
            playedAt = it.longOrNull("played_at"),
            playedDev = it.stringOrNull("played_dev"),
            archived = it.flag("archived"),
            archivedAt = it.longOrNull("archived_at"),
            archivedDev = it.stringOrNull("archived_dev"),
        )
    }

private fun loadSetting(db: SupportSQLiteDatabase, key: String): SettingRow? =
    db.queryOne("select * from setting where key = ?", key) {
        SettingRow(it.string("key"), it.string("value"), it.long("value_at"), it.string("value_dev"))
    }

// ----------------------------------------------------------------------------------------------
// Podcast scopes
// ----------------------------------------------------------------------------------------------

/**
 * Makes the scopes follow the merged subscription table (spec 5.3): every subscribed podcast has
 * a scope (new ones at 0), no other podcast does, and an unsubscribed podcast keeps no catalog
 * rows. `episode_state` rows stay, since they are user state.
 *
 * Judging the merged table rather than the pulled rows means a pulled unsubscribe that lost the
 * merge to a newer local subscribe deletes nothing.
 */
private fun reconcileScopes(db: SupportSQLiteDatabase) {
    db.exec(
        """insert into sync_scope (scope, version, complete)
               select 'podcast:' || podcast_id, 0, 0 from subscription where subscribed = 1
               on conflict (scope) do nothing""",
    )
    db.exec(
        """delete from sync_scope where scope like 'podcast:%'
               and substr(scope, 9) not in (select podcast_id from subscription where subscribed = 1)""",
    )
    db.exec("delete from episode where podcast_id in (select podcast_id from subscription where subscribed = 0)")
    db.exec("delete from podcast where id in (select podcast_id from subscription where subscribed = 0)")
}

private fun applyPodcastScope(db: SupportSQLiteDatabase, scope: PodcastScope, inserted: MutableSet<String>) {
    val key = "podcast:${scope.id}"
    val changes = when (scope) {
        is PodcastScope.NotSubscribed -> {
            db.exec("delete from sync_scope where scope = ?", key)
            return
        }
        is PodcastScope.Changes -> scope
    }
    // The reconcile above removed the scope when the podcast's unsubscribe is still in the
    // outbox. The server, which hasn't seen it, still serves the scope, and writing the page
    // would bring back catalog rows the scope rule forbids.
    db.queryOne("select 1 from sync_scope where scope = ?", key) { } ?: return

    changes.podcast?.let { podcast ->
        // The server's columns are written and the placeholder's `local_*` ones kept. Only a
        // failing podcast fills `failing_since` and `feed_error` (spec 5.5).
        val failing = podcast.status == PodcastStatus.Failing
        db.exec(
            """insert into podcast
                   (id, feed_url, status, failing_since, feed_error, title, author, description,
                    link, artwork_url, language, version)
               values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               on conflict (id) do update set
                   feed_url = excluded.feed_url, status = excluded.status,
                   failing_since = excluded.failing_since, feed_error = excluded.feed_error,
                   title = excluded.title, author = excluded.author,
                   description = excluded.description, link = excluded.link,
                   artwork_url = excluded.artwork_url, language = excluded.language,
                   version = excluded.version""",
            podcast.id, podcast.feedUrl, podcast.status.wire,
            podcast.failingSince.takeIf { failing },
            podcast.error?.takeIf { failing }?.let { WireJson.encodeToString(FeedError.serializer(), it) },
            podcast.title, podcast.author, podcast.description, podcast.link, podcast.artworkUrl,
            podcast.language, podcast.version,
        )
    }
    for (episode in changes.episodes) {
        when (episode) {
            is Episode -> {
                if (db.queryOne("select 1 from episode where id = ?", episode.id) { } == null) inserted += episode.id
                db.exec(
                    """insert or replace into episode
                           (id, podcast_id, title, summary_html, content_html, link, published_at,
                            duration_ms, audio_url, audio_type, audio_bytes, artwork_url, season, number, version)
                       values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                    episode.id, episode.podcastId, episode.title, episode.summaryHtml, episode.contentHtml,
                    episode.link, episode.publishedAt, episode.durationMs, episode.audioUrl, episode.audioType,
                    episode.audioBytes, episode.artworkUrl, episode.season, episode.number, episode.version,
                )
            }
            is EpisodeTombstone -> db.exec("delete from episode where id = ?", episode.id)
        }
    }
    setScope(db, key, changes.version, changes.complete)
}

private fun setScope(db: SupportSQLiteDatabase, scope: String, version: Long, complete: Boolean) {
    db.exec("insert or replace into sync_scope (scope, version, complete) values (?, ?, ?)", scope, version, complete)
}
