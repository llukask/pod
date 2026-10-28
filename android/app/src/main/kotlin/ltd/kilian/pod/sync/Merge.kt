package ltd.kilian.pod.sync

import ltd.kilian.pod.core.EpisodeState
import ltd.kilian.pod.core.Setting
import ltd.kilian.pod.core.Stamp
import ltd.kilian.pod.core.Subscription
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.core.wins
import ltd.kilian.pod.store.EpisodeStateRow
import ltd.kilian.pod.store.SettingRow
import ltd.kilian.pod.store.SubscriptionRow

// ==============================================================================================
// Merge (spec 5.4)
// ==============================================================================================
//
// Pulled user-state rows always go through these, never a plain overwrite, so a local edit still
// in the outbox (which carries a newer `at` than the server row) survives the pull. Each LWW
// field is decided on its own: a pulled row can win `position` and lose `played` at once. Local
// user actions don't merge; they overwrite (spec 5.2, `LocalWrite`).

/**
 * `feed_url` has no stamp and moves with `subscribed`. `replaced_by` isn't LWW: the pulled value
 * is the server's current word and is always taken; acting on it is `followReplacement`'s job.
 */
fun merge(local: SubscriptionRow?, remote: Subscription): SubscriptionRow {
    val pulled = SubscriptionRow(
        podcastId = remote.podcastId,
        feedUrl = remote.feedUrl,
        subscribed = remote.subscribed,
        subscribedAt = remote.subscribedAt,
        subscribedDev = remote.subscribedDev,
        replacedBy = remote.replacedBy,
    )
    if (local == null) return pulled
    val remoteWins = wins(Stamp(remote.subscribedAt, remote.subscribedDev), Stamp(local.subscribedAt, local.subscribedDev))
    return if (remoteWins) pulled else local.copy(replacedBy = remote.replacedBy)
}

/**
 * A pulled field that was never written (null stamp) never replaces a local one: `wins` has no
 * case for a null new stamp, and the server only sends null for a field nobody wrote.
 */
fun merge(local: EpisodeStateRow?, remote: EpisodeState): EpisodeStateRow {
    var row = local ?: return EpisodeStateRow(
        episodeId = remote.episodeId,
        podcastId = remote.podcastId,
        positionMs = remote.positionMs,
        positionAt = remote.positionAt,
        positionDev = remote.positionDev,
        played = remote.played,
        playedAt = remote.playedAt,
        playedDev = remote.playedDev,
        archived = remote.archived,
        archivedAt = remote.archivedAt,
        archivedDev = remote.archivedDev,
    )
    if (pulledWins(remote.positionAt, remote.positionDev, row.positionAt, row.positionDev)) {
        row = row.copy(positionMs = remote.positionMs, positionAt = remote.positionAt, positionDev = remote.positionDev)
    }
    if (pulledWins(remote.playedAt, remote.playedDev, row.playedAt, row.playedDev)) {
        row = row.copy(played = remote.played, playedAt = remote.playedAt, playedDev = remote.playedDev)
    }
    if (pulledWins(remote.archivedAt, remote.archivedDev, row.archivedAt, row.archivedDev)) {
        row = row.copy(archived = remote.archived, archivedAt = remote.archivedAt, archivedDev = remote.archivedDev)
    }
    return row
}

fun merge(local: SettingRow?, remote: Setting): SettingRow {
    val pulled = SettingRow(remote.key, WireJson.encodeToString(remote.value), remote.valueAt, remote.valueDev)
    if (local == null) return pulled
    return if (wins(Stamp(remote.valueAt, remote.valueDev), Stamp(local.valueAt, local.valueDev))) pulled else local
}

private fun pulledWins(at: Long?, dev: String?, localAt: Long?, localDev: String?): Boolean {
    val pulled = Stamp.fromColumns(at, dev) ?: return false
    return wins(pulled, Stamp.fromColumns(localAt, localDev))
}
