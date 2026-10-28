package ltd.kilian.pod.store

import androidx.room.ColumnInfo
import androidx.room.Entity
import androidx.room.Index
import androidx.room.PrimaryKey

// ==============================================================================================
// The local schema: spec 5.1 (sync mirror, outbox, account), 7.1 (device-local download state),
// the rejected-mutation log of 5.6 and the artwork cache of 7.5.
// ==============================================================================================
//
// Table and column names, types and defaults follow the spec's SQLite DDL, which pod-desktop
// uses verbatim, so the conformance fixtures' rows map onto these tables column for column.
// Timestamps are epoch ms. Room can't declare CHECK constraints, so `account.id = 1` and
// `download_wish.source in ('pin', 'auto')` are upheld by the code that writes those rows.

@Entity(tableName = "podcast")
data class PodcastRow(
    @PrimaryKey val id: String,
    @ColumnInfo(name = "feed_url") val feedUrl: String,
    val status: String,
    /** Failing rows only (spec 4.2). */
    @ColumnInfo(name = "failing_since") val failingSince: Long? = null,
    /** The wire `FeedError` object as JSON, failing rows only. */
    @ColumnInfo(name = "feed_error") val feedError: String? = null,
    val title: String? = null,
    val author: String? = null,
    val description: String? = null,
    val link: String? = null,
    @ColumnInfo(name = "artwork_url") val artworkUrl: String? = null,
    val language: String? = null,
    /** 0 for a local placeholder (spec 5.5). */
    val version: Long,
    /** From directory search, shown while pending. */
    @ColumnInfo(name = "local_title") val localTitle: String? = null,
    @ColumnInfo(name = "local_artwork_url") val localArtworkUrl: String? = null,
)

@Entity(
    tableName = "episode",
    indices = [
        Index(name = "episode_podcast_pub", value = ["podcast_id", "published_at"], orders = [Index.Order.ASC, Index.Order.DESC]),
        Index(name = "episode_pub", value = ["published_at"], orders = [Index.Order.DESC]),
    ],
)
data class EpisodeRow(
    @PrimaryKey val id: String,
    @ColumnInfo(name = "podcast_id") val podcastId: String,
    val title: String,
    @ColumnInfo(name = "summary_html") val summaryHtml: String,
    @ColumnInfo(name = "content_html") val contentHtml: String,
    val link: String?,
    @ColumnInfo(name = "published_at") val publishedAt: Long,
    @ColumnInfo(name = "duration_ms") val durationMs: Long?,
    @ColumnInfo(name = "audio_url") val audioUrl: String,
    @ColumnInfo(name = "audio_type") val audioType: String,
    @ColumnInfo(name = "audio_bytes") val audioBytes: Long?,
    @ColumnInfo(name = "artwork_url") val artworkUrl: String?,
    val season: Int?,
    val number: Int?,
    val version: Long,
)

/** No foreign key: an alias row (`replaced_by`) names a podcast id the server never creates. */
@Entity(tableName = "subscription")
data class SubscriptionRow(
    @PrimaryKey @ColumnInfo(name = "podcast_id") val podcastId: String,
    @ColumnInfo(name = "feed_url") val feedUrl: String,
    val subscribed: Boolean,
    @ColumnInfo(name = "subscribed_at") val subscribedAt: Long,
    @ColumnInfo(name = "subscribed_dev") val subscribedDev: String,
    @ColumnInfo(name = "replaced_by") val replacedBy: String? = null,
)

/**
 * No foreign key to `episode`: a state can arrive in the user scope before its podcast scope
 * has synced the episode, and must be kept (spec 5.1).
 */
@Entity(
    tableName = "episode_state",
    indices = [Index(name = "episode_state_played", value = ["played", "archived"])],
)
data class EpisodeStateRow(
    @PrimaryKey @ColumnInfo(name = "episode_id") val episodeId: String,
    @ColumnInfo(name = "podcast_id") val podcastId: String,
    @ColumnInfo(name = "position_ms", defaultValue = "0") val positionMs: Long = 0,
    @ColumnInfo(name = "position_at") val positionAt: Long? = null,
    @ColumnInfo(name = "position_dev") val positionDev: String? = null,
    @ColumnInfo(defaultValue = "0") val played: Boolean = false,
    @ColumnInfo(name = "played_at") val playedAt: Long? = null,
    @ColumnInfo(name = "played_dev") val playedDev: String? = null,
    @ColumnInfo(defaultValue = "0") val archived: Boolean = false,
    @ColumnInfo(name = "archived_at") val archivedAt: Long? = null,
    @ColumnInfo(name = "archived_dev") val archivedDev: String? = null,
)

@Entity(tableName = "setting")
data class SettingRow(
    @PrimaryKey val key: String,
    /** JSON. */
    val value: String,
    @ColumnInfo(name = "value_at") val valueAt: Long,
    @ColumnInfo(name = "value_dev") val valueDev: String,
)

@Entity(tableName = "sync_scope")
data class SyncScopeRow(
    /** `user` or `podcast:<id>`. */
    @PrimaryKey val scope: String,
    val version: Long,
    @ColumnInfo(defaultValue = "0") val complete: Boolean = false,
)

@Entity(tableName = "outbox", indices = [Index(name = "outbox_key", value = ["coalesce_key"])])
data class OutboxRow(
    @PrimaryKey @ColumnInfo(name = "mutation_id") val mutationId: String,
    /** e.g. `position:<episode_id>` (spec 5.2). */
    @ColumnInfo(name = "coalesce_key") val coalesceKey: String,
    /** The JSON mutation as sent on the wire. */
    val payload: String,
    @ColumnInfo(name = "created_at") val createdAt: Long,
    @ColumnInfo(defaultValue = "0") val attempts: Int = 0,
)

/** The single signed-in account; `id` is always 1. */
@Entity(tableName = "account")
data class AccountRow(
    @PrimaryKey val id: Int = 1,
    @ColumnInfo(name = "server_url") val serverUrl: String,
    @ColumnInfo(name = "user_id") val userId: String,
    val username: String,
    @ColumnInfo(name = "device_id") val deviceId: String,
    val token: String,
) {
    override fun toString() = "AccountRow(serverUrl=$serverUrl, userId=$userId, username=$username, deviceId=$deviceId, token=<redacted>)"
}

@Entity(tableName = "podcast_device")
data class PodcastDeviceRow(
    @PrimaryKey @ColumnInfo(name = "podcast_id") val podcastId: String,
    @ColumnInfo(defaultValue = "0") val notify: Boolean = false,
    /** Keep the newest N unplayed, 0 = off. */
    @ColumnInfo(name = "auto_keep", defaultValue = "0") val autoKeep: Int = 0,
    @ColumnInfo(name = "auto_on_metered", defaultValue = "0") val autoOnMetered: Boolean = false,
)

@Entity(tableName = "download_wish", primaryKeys = ["episode_id", "source"])
data class DownloadWishRow(
    @ColumnInfo(name = "episode_id") val episodeId: String,
    /** `pin` or `auto`. */
    val source: String,
    @ColumnInfo(name = "created_at") val createdAt: Long,
    /** Pin only: "download on mobile data anyway". */
    @ColumnInfo(name = "metered_ok", defaultValue = "0") val meteredOk: Boolean = false,
)

@Entity(tableName = "download")
data class DownloadRow(
    @PrimaryKey @ColumnInfo(name = "episode_id") val episodeId: String,
    /** See spec 7.2. */
    val state: String,
    /** The URL this file was fetched from. */
    @ColumnInfo(name = "audio_url") val audioUrl: String,
    /** Relative to the downloads root. */
    val path: String,
    @ColumnInfo(name = "bytes_total") val bytesTotal: Long? = null,
    /** Durable checkpoint, spec 7.3. */
    @ColumnInfo(name = "bytes_done", defaultValue = "0") val bytesDone: Long = 0,
    /** ETag or Last-Modified for If-Range. */
    val validator: String? = null,
    @ColumnInfo(defaultValue = "0") val attempts: Int = 0,
    @ColumnInfo(name = "next_attempt_at") val nextAttemptAt: Long? = null,
    @ColumnInfo(name = "last_error") val lastError: String? = null,
    /** Why a queued row isn't running: connection, wifi, space, budget. */
    @ColumnInfo(name = "wait_reason") val waitReason: String? = null,
    @ColumnInfo(name = "completed_at") val completedAt: Long? = null,
    /** Set when no wish covers the row, cleared when one does. */
    @ColumnInfo(name = "unwanted_since") val unwantedSince: Long? = null,
    /** The transfer that owns the row and its `.part` file. */
    @ColumnInfo(name = "run_id") val runId: String? = null,
)

/**
 * Sizes learned from Content-Length / Content-Range, kept apart from `download` so cancelling
 * or evicting a row doesn't forget them and budget fitting doesn't fall back to a smaller guess.
 */
@Entity(tableName = "known_size")
data class KnownSizeRow(
    @PrimaryKey @ColumnInfo(name = "episode_id") val episodeId: String,
    val bytes: Long,
)

/** Device-local settings (spec 7.1); the one table logout keeps. `value` is JSON. */
@Entity(tableName = "device_setting")
data class DeviceSettingRow(@PrimaryKey val key: String, val value: String)

/**
 * Mutations the server will never accept (spec 5.6): `rejected` results and outbox rows dropped
 * after 5 failed attempts. Settings shows them as "N changes couldn't be saved".
 */
@Entity(tableName = "rejected_mutation")
data class RejectedMutationRow(
    @PrimaryKey @ColumnInfo(name = "mutation_id") val mutationId: String,
    /** The JSON mutation as it sat in the outbox. */
    val payload: String,
    /** A rejection code, or `bad_request` after 5 attempts. */
    val reason: String,
    @ColumnInfo(name = "rejected_at") val rejectedAt: Long,
)

/**
 * The artwork cache (spec 7.5), one row per image file. Spec 7.5 has Android use Coil's disk
 * cache instead; this table keeps the schema identical to desktop's until that is decided.
 */
@Entity(tableName = "artwork")
data class ArtworkRow(
    @PrimaryKey val url: String,
    /** Name under the artwork root. */
    val file: String,
    val bytes: Long,
    /** Epoch ms of the last pass that wanted it, for LRU eviction. */
    @ColumnInfo(name = "last_used") val lastUsed: Long,
)
