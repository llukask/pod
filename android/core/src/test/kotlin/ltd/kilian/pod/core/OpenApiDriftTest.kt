@file:OptIn(ExperimentalSerializationApi::class)

package ltd.kilian.pod.core

import java.io.File
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.DynamicTest.dynamicTest
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.TestFactory

/**
 * Checks the hand-written wire types against `openapi.json`, which pod-server's tests keep in step
 * with pod-protocol (spec 11). For each schema, the Kotlin class must have exactly its properties,
 * and a property is required in the schema exactly when the Kotlin field has no default. A tag
 * (`type`, `status`, `deleted`) is the class's discriminator in Kotlin, so it isn't a field there.
 */
class OpenApiDriftTest {
    private val schemas: JsonObject = Json.parseToJsonElement(
        File(System.getProperty("pod.openapi") ?: error("pod.openapi is set by the Gradle test task")).readText()
    ).jsonObject.getValue("components").jsonObject.getValue("schemas").jsonObject

    private class Mapped(val descriptor: SerialDescriptor, val tag: String? = null)

    private val mapped: Map<String, Mapped> = mapOf(
        "AuthRequest" to Mapped(AuthRequest.serializer().descriptor),
        "TokenResponse" to Mapped(TokenResponse.serializer().descriptor),
        "MeResponse" to Mapped(MeResponse.serializer().descriptor),
        "DirectorySearchResponse" to Mapped(DirectorySearchResponse.serializer().descriptor),
        "DirectoryResult" to Mapped(DirectoryResult.serializer().descriptor),
        "ErrorBody" to Mapped(ErrorBody.serializer().descriptor),
        "ErrorDetail" to Mapped(ErrorDetail.serializer().descriptor),
        "SubscriptionSet" to Mapped(Mutation.SubscriptionSet.serializer().descriptor, "type"),
        "EpisodePosition" to Mapped(Mutation.EpisodePosition.serializer().descriptor, "type"),
        "EpisodePlayed" to Mapped(Mutation.EpisodePlayed.serializer().descriptor, "type"),
        "EpisodeArchived" to Mapped(Mutation.EpisodeArchived.serializer().descriptor, "type"),
        "SettingSet" to Mapped(Mutation.SettingSet.serializer().descriptor, "type"),
        "SyncRequest" to Mapped(SyncRequest.serializer().descriptor),
        "ScopeCursors" to Mapped(ScopeCursors.serializer().descriptor),
        "PodcastCursor" to Mapped(PodcastCursor.serializer().descriptor),
        "SyncResponse" to Mapped(SyncResponse.serializer().descriptor),
        "MutationResult" to Mapped(MutationResult.serializer().descriptor),
        "ScopeChanges" to Mapped(ScopeChanges.serializer().descriptor),
        "UserScope" to Mapped(UserScope.serializer().descriptor),
        "NotSubscribedScope" to Mapped(PodcastScope.NotSubscribed.serializer().descriptor, "status"),
        "PodcastChanges" to Mapped(PodcastScope.Changes.serializer().descriptor, "status"),
        "Episode" to Mapped(Episode.serializer().descriptor, "deleted"),
        "EpisodeTombstone" to Mapped(EpisodeTombstone.serializer().descriptor, "deleted"),
        "Subscription" to Mapped(Subscription.serializer().descriptor),
        "EpisodeState" to Mapped(EpisodeState.serializer().descriptor),
        "Setting" to Mapped(Setting.serializer().descriptor),
    )

    /** Schemas without a Kotlin class of their own, each with the reason. */
    private val unmapped: Map<String, String> = mapOf(
        "Podcast" to "allOf with FeedStatus; checked by podcastFlattensFeedStatus",
        "FeedStatus" to "flattened into Podcast",
        "FeedError" to "one open class FeedError; checked by feedErrorCarriesStatusOnlyForHttp",
        "Mutation" to "sealed interface Mutation over the five variants",
        "PodcastScope" to "sealed interface PodcastScope",
        "ScopeEpisode" to "sealed interface ScopeEpisode",
        "MutationId" to "plain String", "DeviceId" to "plain String", "UserId" to "plain String",
        "PodcastId" to "plain String", "EpisodeId" to "plain String",
        "Timestamp" to "Long through TimestampSerializer",
        "ErrorCode" to "open set, a String", "RejectionCode" to "open set, a String",
        "PodcastItem" to "read endpoint, web client only", "PodcastList" to "read endpoint, web client only",
        "EpisodeItem" to "read endpoint, web client only", "EpisodePage" to "read endpoint, web client only",
        "InboxItem" to "read endpoint, web client only", "InboxPage" to "read endpoint, web client only",
        "SettingsResponse" to "read endpoint, web client only",
        "SonosPlayersResponse" to "Sonos, no client yet", "SonosPlayer" to "Sonos, no client yet",
        "SonosPlayerState" to "Sonos, no client yet", "SonosSessionView" to "Sonos, no client yet",
        "SonosPlayRequest" to "Sonos, no client yet", "SonosCommand" to "Sonos, no client yet",
        "SonosPause" to "Sonos, no client yet", "SonosResume" to "Sonos, no client yet",
        "SonosSeek" to "Sonos, no client yet", "SonosSkip" to "Sonos, no client yet",
        "SonosVolume" to "Sonos, no client yet", "SonosReleased" to "Sonos, no client yet",
    )

    @Test
    fun everySchemaIsAccountedFor() {
        assertEquals(schemas.keys, mapped.keys + unmapped.keys, "schemas in openapi.json vs this test's tables")
    }

    @TestFactory
    fun fieldsMatch(): List<DynamicTest> = mapped.map { (name, kotlin) ->
        dynamicTest(name) {
            val schema = schemas.getValue(name).jsonObject
            checkFields(name, schema.properties(), schema.required(), kotlin.descriptor, kotlin.tag)
        }
    }

    @Test
    fun podcastFlattensFeedStatus() {
        val parts = schemas.getValue("Podcast").jsonObject.getValue("allOf").jsonArray.map { it.jsonObject }
        val own = parts.single { "properties" in it }
        val statusVariants = schemas.getValue("FeedStatus").jsonObject.getValue("oneOf").jsonArray.map { it.jsonObject }
        val properties = own.properties() + statusVariants.flatMap { it.properties() }
        // `status` is required in every variant; `failing_since` and `error` only in `failing`.
        checkFields("Podcast", properties, own.required() + "status", Podcast.serializer().descriptor, tag = null)
    }

    @Test
    fun feedErrorCarriesStatusOnlyForHttp() {
        val variants = schemas.getValue("FeedError").jsonObject.getValue("oneOf").jsonArray.map { it.jsonObject }
        val extra = variants.flatMap { it.properties() - "code" }.toSet()
        assertEquals(setOf("status"), extra)
        checkFields("FeedError", setOf("code", "status"), setOf("code"), FeedError.serializer().descriptor, tag = null)
    }

    private fun JsonObject.properties(): Set<String> = get("properties")?.jsonObject?.keys ?: emptySet()

    private fun JsonObject.required(): Set<String> =
        get("required")?.jsonArray?.map { it.jsonPrimitive.content }?.toSet() ?: emptySet()

    private fun checkFields(name: String, properties: Set<String>, required: Set<String>, descriptor: SerialDescriptor, tag: String?) {
        val elements = (0 until descriptor.elementsCount).associate { descriptor.getElementName(it) to descriptor.isElementOptional(it) }
        val kotlinFields = elements.keys - setOfNotNull(tag)
        assertEquals(properties - setOfNotNull(tag), kotlinFields, "$name: properties")
        val kotlinRequired = elements.filterValues { optional -> !optional }.keys - setOfNotNull(tag)
        assertEquals(required - setOfNotNull(tag), kotlinRequired, "$name: required properties (Kotlin fields without a default)")
    }
}
