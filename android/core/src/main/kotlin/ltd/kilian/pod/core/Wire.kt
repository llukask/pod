@file:OptIn(ExperimentalSerializationApi::class)

package ltd.kilian.pod.core

import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonClassDiscriminator
import kotlinx.serialization.json.JsonContentPolymorphicSerializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.jsonObject

// ==============================================================================================
// Wire types of spec section 4
// ==============================================================================================
//
// These mirror pod-protocol's types. `openapi.json` at the repo root is generated from those, and
// `OpenApiDriftTest` checks every class here against its schema there, so a field the server
// adds or renames fails this module's tests instead of being silently dropped.
//
// The read endpoints of spec 4.5 serve the online web client only, so their types
// (`PodcastItem`, `EpisodeItem`, `InboxItem`, the pages, `SettingsResponse`) are deliberately
// left out.

/** The JSON configuration every request and response goes through. */
val WireJson: Json = Json {
    // A later server may add fields (spec 4). Unknown fields are ignored rather than failing
    // the whole sync; the drift test is what notices them during development.
    ignoreUnknownKeys = true
    // `null` fields are written, because the server's types treat a missing and a null
    // `Option` the same but the fixtures compare wire bodies exactly.
    explicitNulls = true
    encodeDefaults = true
}

/**
 * An RFC 3339 timestamp on the wire, epoch milliseconds in Kotlin and in the local store.
 * Written with exactly three fraction digits and `Z`, as the server writes them.
 */
typealias Timestamp = @Serializable(with = TimestampSerializer::class) Long

object TimestampSerializer : KSerializer<Long> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("Timestamp", PrimitiveKind.STRING)

    override fun deserialize(decoder: Decoder): Long = Timestamps.parse(decoder.decodeString())

    override fun serialize(encoder: Encoder, value: Long) = encoder.encodeString(Timestamps.format(value))
}

// ----------------------------------------------------------------------------------------------
// Auth and directory (4.1, 4.6)
// ----------------------------------------------------------------------------------------------

@Serializable
data class AuthRequest(
    val username: String,
    val password: String,
    @SerialName("device_name") val deviceName: String,
) {
    override fun toString() = "AuthRequest(username=$username, password=<redacted>, deviceName=$deviceName)"
}

@Serializable
data class TokenResponse(
    val token: String,
    @SerialName("device_id") val deviceId: String,
    @SerialName("user_id") val userId: String,
    @SerialName("expires_at") val expiresAt: Timestamp,
) {
    override fun toString() = "TokenResponse(token=<redacted>, deviceId=$deviceId, userId=$userId)"
}

@Serializable
data class MeResponse(
    @SerialName("user_id") val userId: String,
    val username: String,
    @SerialName("device_id") val deviceId: String,
)

@Serializable
data class DirectorySearchResponse(val results: List<DirectoryResult>)

@Serializable
data class DirectoryResult(
    val title: String,
    val author: String? = null,
    @SerialName("feed_url") val feedUrl: String,
    @SerialName("artwork_url") val artworkUrl: String? = null,
    @SerialName("podcast_id") val podcastId: String,
)

/**
 * The error shape of every non-2xx answer. [ErrorDetail.code] is an open set (spec 4): a
 * client handles a code it doesn't know by the HTTP status alone.
 */
@Serializable
data class ErrorBody(val error: ErrorDetail)

@Serializable
data class ErrorDetail(val code: String, val message: String)

// ----------------------------------------------------------------------------------------------
// Mutations (4.3)
// ----------------------------------------------------------------------------------------------

/**
 * A user action as the server applies it. [at] and [dev] are the stamp the local write gave the
 * field (spec 5.2); [dev] can be a device id from before a re-login.
 */
@Serializable
@JsonClassDiscriminator("type")
sealed interface Mutation {
    val id: String
    val at: Timestamp
    val dev: String

    /** The outbox coalesce key (spec 5.2): one pending mutation per field. */
    val coalesceKey: String

    @Serializable
    @SerialName("subscription.set")
    data class SubscriptionSet(
        override val id: String,
        override val at: Timestamp,
        override val dev: String,
        @SerialName("podcast_id") val podcastId: String,
        @SerialName("feed_url") val feedUrl: String,
        val subscribed: Boolean,
    ) : Mutation {
        override val coalesceKey get() = "sub:$podcastId"
    }

    @Serializable
    @SerialName("episode.position")
    data class EpisodePosition(
        override val id: String,
        override val at: Timestamp,
        override val dev: String,
        @SerialName("episode_id") val episodeId: String,
        @SerialName("position_ms") val positionMs: Long,
    ) : Mutation {
        override val coalesceKey get() = "position:$episodeId"
    }

    @Serializable
    @SerialName("episode.played")
    data class EpisodePlayed(
        override val id: String,
        override val at: Timestamp,
        override val dev: String,
        @SerialName("episode_id") val episodeId: String,
        val played: Boolean,
    ) : Mutation {
        override val coalesceKey get() = "played:$episodeId"
    }

    @Serializable
    @SerialName("episode.archived")
    data class EpisodeArchived(
        override val id: String,
        override val at: Timestamp,
        override val dev: String,
        @SerialName("episode_id") val episodeId: String,
        val archived: Boolean,
    ) : Mutation {
        override val coalesceKey get() = "archived:$episodeId"
    }

    @Serializable
    @SerialName("setting.set")
    data class SettingSet(
        override val id: String,
        override val at: Timestamp,
        override val dev: String,
        val key: String,
        val value: JsonElement,
    ) : Mutation {
        override val coalesceKey get() = "setting:$key"
    }
}

// ----------------------------------------------------------------------------------------------
// Sync (4.2)
// ----------------------------------------------------------------------------------------------

/** At most this many mutations go in one request; more is `413 batch_too_large`. */
const val MAX_MUTATIONS = 200

@Serializable
data class SyncRequest(
    /** Empty for a pull-only request. Always sent, though the server defaults it. */
    val mutations: List<Mutation> = emptyList(),
    /** Absent for a push-only request. */
    @EncodeDefault(EncodeDefault.Mode.NEVER) val scopes: ScopeCursors? = null,
    @EncodeDefault(EncodeDefault.Mode.NEVER) val limit: Int? = null,
)

@Serializable
data class ScopeCursors(val user: Long, val podcasts: List<PodcastCursor> = emptyList())

@Serializable
data class PodcastCursor(val id: String, val version: Long)

@Serializable
data class SyncResponse(
    @SerialName("server_time") val serverTime: Timestamp,
    val results: List<MutationResult>,
    /** Absent when the request had no scopes. */
    @EncodeDefault(EncodeDefault.Mode.NEVER) val scopes: ScopeChanges? = null,
)

@Serializable
enum class MutationStatus {
    @SerialName("applied") Applied,
    @SerialName("superseded") Superseded,
    @SerialName("rejected") Rejected,
}

/**
 * One mutation's outcome. [at] is the stamp the server stored, clamped (spec 4.4); only a
 * rejection of an unreadable `at` lacks it. [code] is set exactly for [MutationStatus.Rejected]
 * and is an open set like the error codes.
 */
@Serializable
data class MutationResult(
    val id: String,
    val status: MutationStatus,
    @EncodeDefault(EncodeDefault.Mode.NEVER) val at: Timestamp? = null,
    @EncodeDefault(EncodeDefault.Mode.NEVER) val code: String? = null,
)

@Serializable
data class ScopeChanges(val user: UserScope, val podcasts: List<PodcastScope>)

@Serializable
data class UserScope(
    val version: Long,
    val complete: Boolean,
    val subscriptions: List<Subscription>,
    @SerialName("episode_states") val episodeStates: List<EpisodeState>,
    val settings: List<Setting>,
)

@Serializable
@JsonClassDiscriminator("status")
sealed interface PodcastScope {
    val id: String

    /** The user isn't subscribed (any more); the client drops the scope. */
    @Serializable
    @SerialName("not_subscribed")
    data class NotSubscribed(override val id: String) : PodcastScope

    @Serializable
    @SerialName("ok")
    data class Changes(
        override val id: String,
        val version: Long,
        val complete: Boolean,
        @EncodeDefault(EncodeDefault.Mode.NEVER) val podcast: Podcast? = null,
        @EncodeDefault(EncodeDefault.Mode.NEVER) val episodes: List<ScopeEpisode> = emptyList(),
    ) : PodcastScope
}

// ----------------------------------------------------------------------------------------------
// Entities (4.2)
// ----------------------------------------------------------------------------------------------

@Serializable
enum class PodcastStatus {
    @SerialName("pending") Pending,
    @SerialName("ok") Ok,
    @SerialName("failing") Failing,
    @SerialName("gone") Gone,
    ;

    /** The tag as the wire and the local `podcast.status` column spell it. */
    val wire: String get() = serializer().descriptor.getElementName(ordinal)
}

/**
 * Why a feed is failing (spec 3.2), tagged by [code]. Only `http` carries [status]. Kept as one
 * open class rather than an enum, so a code a later server adds still reaches the UI, worded
 * generically.
 */
@Serializable
data class FeedError(val code: String, @EncodeDefault(EncodeDefault.Mode.NEVER) val status: Int? = null)

/**
 * The catalog row of a podcast scope. On the wire the feed status is flattened into it and
 * tagged by `status`; only [PodcastStatus.Failing] carries [failingSince] and [error].
 */
@Serializable
data class Podcast(
    val id: String,
    @SerialName("feed_url") val feedUrl: String,
    val status: PodcastStatus,
    @EncodeDefault(EncodeDefault.Mode.NEVER) @SerialName("failing_since") val failingSince: Timestamp? = null,
    @EncodeDefault(EncodeDefault.Mode.NEVER) val error: FeedError? = null,
    val title: String? = null,
    val author: String? = null,
    val description: String? = null,
    val link: String? = null,
    @SerialName("artwork_url") val artworkUrl: String? = null,
    val language: String? = null,
    val version: Long,
)

/** A podcast scope's episode row: a live episode or a tombstone, told apart by `deleted`. */
@Serializable(with = ScopeEpisodeSerializer::class)
sealed interface ScopeEpisode {
    val id: String
    val version: Long
}

@Serializable
data class Episode(
    override val id: String,
    @SerialName("podcast_id") val podcastId: String,
    val title: String,
    @SerialName("summary_html") val summaryHtml: String,
    @SerialName("content_html") val contentHtml: String,
    val link: String? = null,
    @SerialName("published_at") val publishedAt: Timestamp,
    @SerialName("duration_ms") val durationMs: Long? = null,
    @SerialName("audio_url") val audioUrl: String,
    @SerialName("audio_type") val audioType: String,
    @SerialName("audio_bytes") val audioBytes: Long? = null,
    @SerialName("artwork_url") val artworkUrl: String? = null,
    val season: Int? = null,
    val number: Int? = null,
    override val version: Long,
    val deleted: Boolean = false,
) : ScopeEpisode

@Serializable
data class EpisodeTombstone(
    override val id: String,
    override val version: Long,
    val deleted: Boolean = true,
) : ScopeEpisode

object ScopeEpisodeSerializer : JsonContentPolymorphicSerializer<ScopeEpisode>(ScopeEpisode::class) {
    override fun selectDeserializer(element: JsonElement): DeserializationStrategy<ScopeEpisode> =
        when ((element.jsonObject["deleted"] as? JsonPrimitive)?.booleanOrNull) {
            false -> Episode.serializer()
            true -> EpisodeTombstone.serializer()
            null -> error("episode needs a boolean `deleted` field")
        }
}

@Serializable
data class Subscription(
    @SerialName("podcast_id") val podcastId: String,
    @SerialName("feed_url") val feedUrl: String,
    val subscribed: Boolean,
    @SerialName("subscribed_at") val subscribedAt: Timestamp,
    @SerialName("subscribed_dev") val subscribedDev: String,
    /** Set by the server on an alias redirect (spec 4.4); not an LWW field. */
    @SerialName("replaced_by") val replacedBy: String? = null,
    val version: Long,
)

@Serializable
data class EpisodeState(
    @SerialName("episode_id") val episodeId: String,
    @SerialName("podcast_id") val podcastId: String,
    @SerialName("position_ms") val positionMs: Long,
    @SerialName("position_at") val positionAt: Timestamp? = null,
    @SerialName("position_dev") val positionDev: String? = null,
    val played: Boolean,
    @SerialName("played_at") val playedAt: Timestamp? = null,
    @SerialName("played_dev") val playedDev: String? = null,
    val archived: Boolean,
    @SerialName("archived_at") val archivedAt: Timestamp? = null,
    @SerialName("archived_dev") val archivedDev: String? = null,
    val version: Long,
)

@Serializable
data class Setting(
    val key: String,
    val value: JsonElement,
    @SerialName("value_at") val valueAt: Timestamp,
    @SerialName("value_dev") val valueDev: String,
    val version: Long,
)
