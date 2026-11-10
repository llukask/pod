package ltd.kilian.pod.podcast

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import ltd.kilian.pod.inbox.episodeTypeLabel
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

// Spec 16: a serial show and a chosen season start oldest first, anything else newest first, and
// trailers and bonus episodes are labelled.

@RunWith(RobolectricTestRunner::class)
class OrderTest {
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java)
        .allowMainThreadQueries().build()
    private val db get() = database.openHelper.writableDatabase
    private val show = "22222222-2222-5222-8222-222222222222"

    @After
    fun close() = database.close()

    private fun subscribe(showType: String) {
        db.exec(
            "insert into podcast (id, feed_url, status, title, show_type, version) values (?, 'https://example.com/f', 'ok', 'Serial', ?, 1)",
            show, showType,
        )
        db.exec("insert into subscription (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev) values (?, 'https://example.com/f', 1, 0, 'd')", show)
    }

    private fun episode(id: String, publishedAt: Long, season: Int?, type: String = "full") = db.exec(
        """insert into episode (id, podcast_id, title, summary_html, content_html, published_at, audio_url, audio_type, season, episode_type, version)
           values (?, ?, ?, '', '', ?, ?, 'audio/mpeg', ?, ?, 1)""",
        id, show, "Episode $id", publishedAt, "https://example.com/$id.mp3", season, type,
    )

    @Test
    fun aSerialShowOrAChosenSeasonStartsOldestFirst() {
        assertEquals(EpisodeOrder.NewestFirst, defaultOrder(serial = false, season = null))
        assertEquals(EpisodeOrder.OldestFirst, defaultOrder(serial = true, season = null))
        assertEquals(EpisodeOrder.OldestFirst, defaultOrder(serial = false, season = 2))
        assertEquals(EpisodeOrder.NewestFirst, EpisodeOrder.OldestFirst.flipped)
        assertEquals("Oldest first", EpisodeOrder.OldestFirst.label)
    }

    @Test
    fun theListRunsEitherWayWithOrWithoutASeason() {
        subscribe("serial")
        episode("trailer", 100, 1, type = "trailer")
        episode("s1e1", 200, 1)
        episode("s2e1", 300, 2)
        episode("bonus", 400, null, type = "bonus")

        val page = loadPage(db, show)!!
        assertTrue(page.serial)
        assertEquals(listOf("trailer", "s1e1", "s2e1", "bonus"), listedEpisodes(page.episodes, null, EpisodeOrder.OldestFirst).map { it.id })
        assertEquals(listOf("bonus", "s2e1", "s1e1", "trailer"), listedEpisodes(page.episodes, null, EpisodeOrder.NewestFirst).map { it.id })
        assertEquals(listOf("s1e1", "trailer"), listedEpisodes(page.episodes, 1, EpisodeOrder.NewestFirst).map { it.id })
        assertEquals(listOf("trailer", "full", "full", "bonus"), listedEpisodes(page.episodes, null, EpisodeOrder.OldestFirst).map { it.episodeType })
    }

    @Test
    fun anUnknownShowTypeIsEpisodic() {
        subscribe("limited")
        assertFalse(loadPage(db, show)!!.serial)
    }

    @Test
    fun trailersAndBonusEpisodesAreLabelled() {
        assertEquals("Trailer", episodeTypeLabel("trailer"))
        assertEquals("Bonus", episodeTypeLabel("bonus"))
        assertNull(episodeTypeLabel("full"))
        assertNull(episodeTypeLabel("teaser"))
    }
}
