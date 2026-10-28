package ltd.kilian.pod.core

import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.DynamicTest.dynamicTest
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.TestFactory

/** `conformance/ids.json`: feed URL normalization and the deterministic ids of spec section 1. */
class IdsConformanceTest {
    private val ids = fixture("ids.json")

    @Test
    fun namespaces() {
        val namespaces = ids.getValue("namespaces").jsonObject
        assertEquals(namespaces.string("podcast"), Ids.NS_PODCAST.toString())
        assertEquals(namespaces.string("episode"), Ids.NS_EPISODE.toString())
    }

    @TestFactory
    fun feedUrls(): List<DynamicTest> = ids.getValue("feed_urls").jsonArray.map { case ->
        val row = case.jsonObject
        dynamicTest(case.name) {
            val result = FeedUrl.normalize(row.string("input"))
            val url = (result as? FeedUrlResult.Ok)?.url ?: error("expected Ok, got $result")
            assertEquals(row.string("feed_url"), url.feedUrl, "feed_url")
            assertEquals(row.string("normalized"), url.normalized, "normalized")
            assertEquals(row.string("podcast_id"), url.podcastId, "podcast_id")
        }
    }

    @TestFactory
    fun rejectedFeedUrls(): List<DynamicTest> = ids.getValue("rejected_feed_urls").jsonArray.map { case ->
        val row = case.jsonObject
        dynamicTest(case.name) {
            val result = FeedUrl.normalize(row.string("input"))
            val error = (result as? FeedUrlResult.Err)?.error ?: error("expected Err, got $result")
            assertEquals(row.string("error"), error.code)
        }
    }

    @TestFactory
    fun episodeKeys(): List<DynamicTest> = ids.getValue("episode_keys").jsonArray.map { case ->
        val row = case.jsonObject
        dynamicTest(case.name) {
            assertEquals(row.string("key"), Ids.episodeKey(row.stringOrNull("guid"), row.string("enclosure_url")))
        }
    }

    @TestFactory
    fun episodeIds(): List<DynamicTest> = ids.getValue("episode_ids").jsonArray.map { case ->
        val row = case.jsonObject
        dynamicTest(case.name) {
            assertEquals(row.string("episode_id"), Ids.episodeId(row.string("podcast_id"), row.string("key")))
        }
    }
}
