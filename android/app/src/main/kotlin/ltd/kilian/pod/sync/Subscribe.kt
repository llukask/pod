package ltd.kilian.pod.sync

import androidx.sqlite.db.SupportSQLiteDatabase
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import ltd.kilian.pod.core.NormalizedFeedUrl
import ltd.kilian.pod.core.Mutation
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.SubscriptionRow
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.string

// ==============================================================================================
// Subscribing locally (spec 5.5)
// ==============================================================================================
//
// [subscribe] works without a network: one local transaction writes a `pending` placeholder,
// the subscription, its outbox mutation and a scope at 0, so the show appears at once and fills
// in once a sync reaches the server and the poller has fetched the feed.

/** What a directory search result says about a show, shown on its placeholder until the server fetches the feed. */
data class DirectoryHint(val title: String, val artworkUrl: String?)

/**
 * Subscribes to [feedUrl] locally and asks the sync engine to send it. Returns the podcast id
 * derived from the URL. The URL arrives normalized: one that doesn't normalize is the caller's
 * inline error (spec 5.5 step 1), and no placeholder or mutation exists for it.
 */
suspend fun subscribe(database: PodDatabase, sync: SyncHandle, feedUrl: NormalizedFeedUrl, hint: DirectoryHint?): String {
    val change = LocalChange.Subscription(feedUrl.podcastId, feedUrl.feedUrl, subscribed = true)
    val now = System.currentTimeMillis()
    withContext(Dispatchers.IO) {
        database.runInTransaction {
            val db = database.openHelper.writableDatabase
            // A podcast row that is already there came from a sync and stays: it shows more than
            // a placeholder can.
            db.exec(
                """insert into podcast (id, feed_url, status, version, local_title, local_artwork_url)
                   values (?, ?, 'pending', 0, ?, ?)
                   on conflict (id) do nothing""",
                feedUrl.podcastId, feedUrl.feedUrl, hint?.title, hint?.artworkUrl,
            )
            sync.inFlight.withLock { inFlight -> applyLocalChange(db, change, newMutationId(now), inFlight, now) }
            db.exec("insert into sync_scope (scope, version) values (?, 0) on conflict (scope) do nothing", "podcast:${feedUrl.podcastId}")
        }
    }
    sync.requestSync(change.syncDelayMs)
    return feedUrl.podcastId
}

/**
 * The subscribed shows whose feed the server hasn't fetched yet: a local placeholder, or a
 * pulled row the server still reports `pending`. The engine syncs again soon while there are
 * any.
 */
fun awaitingFirstFetch(db: SupportSQLiteDatabase): Set<String> =
    db.queryList(
        """select s.podcast_id from subscription s join podcast p on p.id = s.podcast_id
           where s.subscribed = 1 and p.status = 'pending'""",
    ) { it.string("podcast_id") }.toSet()

// ==============================================================================================
// How the server settles a subscribe (spec 5.5)
// ==============================================================================================
//
// - [followReplacement]: the URL is an alias of a podcast the server already knows, so the
//   pulled row for the id the client derived (X) names the canonical podcast (Y) in
//   `replaced_by`. The client moves everything it holds for X over to Y.
// - [forgetRejectedSubscribe]: the server refuses the URL (`invalid_feed_url`) on grounds the
//   client can't check, such as a blocked address. The placeholder goes away.
//
// Both run inside the apply step's transaction. Ports of pod-desktop's `sync/subscribe.rs`.

/**
 * Moves what this device holds for [replaced] (X) to [canonical] (Y), after the pull merged a
 * subscription row for X that carries `replaced_by = canonical`. Runs whatever the merge decided.
 */
fun followReplacement(db: SupportSQLiteDatabase, replaced: String, canonical: String) {
    val alias = loadSubscription(db, replaced) ?: error("the pulled alias row $replaced is stored")

    // 1. Carry X's subscription fields into Y when X's stamp is strictly newer, the server's rule
    // for the same carry (spec 4.4, "Alias redirect" step 1). Not `merge`: its same-device tie
    // rule would let X undo a later mutation for Y that the server clamped into the same ms.
    val target = loadSubscription(db, canonical)
    if (target == null || carries(alias, target)) {
        db.exec(
            """insert into subscription (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
               values (?, ?, ?, ?, ?)
               on conflict (podcast_id) do update set
                   feed_url = excluded.feed_url, subscribed = excluded.subscribed,
                   subscribed_at = excluded.subscribed_at, subscribed_dev = excluded.subscribed_dev""",
            canonical, alias.feedUrl, alias.subscribed, alias.subscribedAt, alias.subscribedDev,
        )
    }

    // 2. Point queued mutations for X at Y, coalesce key included, so a later action on Y
    // replaces them. Only `subscription.set` names a podcast. The mutations of the request this
    // page answers already left the outbox with their results; the server redirects those.
    val queued = db.queryList("select mutation_id, payload from outbox where coalesce_key = ?", "sub:$replaced") {
        it.string("mutation_id") to it.string("payload")
    }
    for ((mutationId, payload) in queued) {
        val mutation = WireJson.decodeFromString(Mutation.serializer(), payload)
        val redirected = if (mutation is Mutation.SubscriptionSet) mutation.copy(podcastId = canonical) else mutation
        db.exec(
            "update outbox set coalesce_key = ?, payload = ? where mutation_id = ?",
            redirected.coalesceKey, WireJson.encodeToString(Mutation.serializer(), redirected), mutationId,
        )
    }

    // 3. This device's settings for the show follow it, unless Y has its own.
    db.exec(
        """insert into podcast_device (podcast_id, notify, auto_keep, auto_on_metered)
               select ?, notify, auto_keep, auto_on_metered from podcast_device where podcast_id = ?
               on conflict (podcast_id) do nothing""",
        canonical, replaced,
    )

    // 4. A placeholder becomes Y's while Y has no row of its own, so the list keeps the directory
    // title until Y's first page lands. The scope rule removes it again should Y end up
    // unsubscribed.
    db.exec(
        "update podcast set id = ? where id = ? and version = 0 and not exists (select 1 from podcast where id = ?)",
        canonical, replaced, canonical,
    )

    // 5. Nothing is left under X. `episode_state` rows stay, since they are user state.
    db.exec("delete from podcast where id = ?", replaced)
    db.exec("delete from episode where podcast_id = ?", replaced)
    db.exec("delete from podcast_device where podcast_id = ?", replaced)
    db.exec("delete from subscription where podcast_id = ?", replaced)
    db.exec("delete from sync_scope where scope = ?", "podcast:$replaced")
}

/** X's stamp is strictly newer: a later `at`, or the same `at` and a greater device. */
private fun carries(alias: SubscriptionRow, canonical: SubscriptionRow): Boolean =
    alias.subscribedAt > canonical.subscribedAt ||
        (alias.subscribedAt == canonical.subscribedAt && alias.subscribedDev > canonical.subscribedDev)

/** Deletes the placeholder, scope and subscription row of a subscribe the server rejected. */
fun forgetRejectedSubscribe(db: SupportSQLiteDatabase, sent: Mutation.SubscriptionSet) {
    // The server never fetch-checks an unsubscribe, and a placeholder only comes with a subscribe.
    if (!sent.subscribed) return
    // A newer local action on the show replaced the rejected one and stands: it is still in the
    // outbox and gets its own answer.
    val removed = db.exec(
        "delete from subscription where podcast_id = ? and subscribed = 1 and subscribed_at = ? and subscribed_dev = ?",
        sent.podcastId, sent.at, sent.dev,
    )
    if (removed == 0) return
    db.exec("delete from podcast where id = ?", sent.podcastId)
    db.exec("delete from episode where podcast_id = ?", sent.podcastId)
    db.exec("delete from sync_scope where scope = ?", "podcast:${sent.podcastId}")
}
