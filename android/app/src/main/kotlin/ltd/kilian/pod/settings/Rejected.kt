package ltd.kilian.pod.settings

import androidx.sqlite.db.SupportSQLiteDatabase
import ltd.kilian.pod.core.Mutation
import ltd.kilian.pod.core.Settings
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull

// The rejected-change log under Settings › Sync (spec 5.6): changes the server refused, and
// outbox rows dropped after five refused attempts, each with its reason. A port of the
// rejected-log part of pod-desktop's `settings/mod.rs`.

/** A change in the rejected log, in words. */
data class Rejection(val mutationId: String, val change: String, val reason: String)

/** The log, newest first. */
fun loadRejected(db: SupportSQLiteDatabase): List<Rejection> =
    db.queryList(
        """select rejected.mutation_id, rejected.payload, rejected.reason, episode.title
           from rejected_mutation rejected
           left join episode on episode.id = json_extract(rejected.payload, '$.episode_id')
           order by rejected.rejected_at desc, rejected.mutation_id desc""",
    ) {
        Rejection(
            it.string("mutation_id"),
            describeChange(it.string("payload"), it.stringOrNull("title")),
            describeReason(it.string("reason")),
        )
    }

/** The change a logged mutation made, from its JSON payload. [episode] is the episode's title when the store still has it. */
fun describeChange(payload: String, episode: String?): String {
    val mutation = runCatching { WireJson.decodeFromString(Mutation.serializer(), payload) }.getOrNull() ?: return "A change"
    val named = episode?.let { "“$it”" } ?: "an episode"
    return when (mutation) {
        is Mutation.SubscriptionSet -> if (mutation.subscribed) "Subscribe to ${mutation.feedUrl}" else "Unsubscribe from ${mutation.feedUrl}"
        is Mutation.EpisodePosition -> "Save the position in $named"
        is Mutation.EpisodePlayed -> if (mutation.played) "Mark $named played" else "Mark $named unplayed"
        is Mutation.EpisodeArchived -> if (mutation.archived) "Archive $named" else "Unarchive $named"
        is Mutation.SettingSet -> when (mutation.key) {
            Settings.PLAYBACK_SPEED -> "Change the playback speed"
            Settings.SKIP_BACK_MS -> "Change the skip back interval"
            Settings.SKIP_FORWARD_MS -> "Change the skip forward interval"
            Settings.PLAYED_THRESHOLD_MS -> "Change when episodes count as played"
            Settings.INBOX_WINDOW_DAYS -> "Change how far back the inbox goes"
            else -> "Change the setting “${mutation.key}”"
        }
    }
}

/**
 * Why the server refused a change, for a rejection code or the engine's `bad_request` after five
 * refused attempts. The codes are an open set (spec 4.3), so an unknown one reads as a plain
 * refusal.
 */
fun describeReason(reason: String): String = when (reason) {
    "bad_request" -> "The server refused it five times."
    "invalid_mutation" -> "The server didn't understand it."
    "unknown_episode" -> "The server doesn't know this episode."
    "invalid_feed_url" -> "The server can't use this feed address."
    else -> "The server refused it ($reason)."
}

