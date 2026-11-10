package ltd.kilian.pod.podcast

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

// Spec 15: a show's seasons come from its local episodes, and a season lists oldest first.

@RunWith(RobolectricTestRunner::class)
class SeasonsTest {
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java)
        .allowMainThreadQueries().build()
    private val db get() = database.openHelper.writableDatabase
    private val show = "11111111-1111-5111-8111-111111111111"

    @After
    fun close() = database.close()

    private fun episode(id: String, publishedAt: Long, season: Int?, number: Int? = null) = db.exec(
        """insert into episode (id, podcast_id, title, summary_html, content_html, published_at, audio_url, audio_type, season, number, version)
           values (?, ?, ?, '', '', ?, ?, 'audio/mpeg', ?, ?, 1)""",
        id, show, "Episode $id", publishedAt, "https://example.com/$id.mp3", season, number,
    )

    private fun subscribe() {
        db.exec("insert into podcast (id, feed_url, status, title, version) values (?, 'https://example.com/f', 'ok', 'Revolutions', 1)", show)
        db.exec("insert into subscription (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev) values (?, 'https://example.com/f', 1, 0, 'd')", show)
    }

    @Test
    fun aSeasonListsOldestFirstWithTiesByIdAndItsTrailerFirst() {
        subscribe()
        // The Revolutions feed in miniature: season 1 opens with an unnumbered trailer, an
        // announcement has no season, season 3 skips 2, and two episodes share an instant.
        episode("intro", 100, 1)
        episode("e1-1", 200, 1, 1)
        episode("e1-2b", 300, 1, 3)
        episode("e1-2a", 300, 1, 2)
        episode("update", 400, null)
        episode("e3-1", 500, 3, 1)

        val page = loadPage(db, show)!!
        assertEquals(listOf(1, 3), page.seasons)
        assertEquals(listOf("e3-1", "update", "e1-2b", "e1-2a", "e1-1", "intro"), page.episodes.map { it.id })
        val oldest = EpisodeOrder.OldestFirst
        assertEquals(listOf("intro", "e1-1", "e1-2a", "e1-2b"), listedEpisodes(page.episodes, 1, oldest).map { it.id })
        assertEquals(listOf("e3-1"), listedEpisodes(page.episodes, 3, oldest).map { it.id })
        assertEquals(emptyList<String>(), listedEpisodes(page.episodes, 2, oldest).map { it.id })
    }

    @Test
    fun aShowWithoutSeasonTagsHasNone() {
        subscribe()
        episode("a", 100, null, 1)
        episode("b", 200, null, 2)
        assertEquals(emptyList<Int>(), loadPage(db, show)!!.seasons)
    }

    @Test
    fun labelsNameWhatTheFeedGave() {
        assertEquals("All seasons", seasonLabel(null))
        assertEquals("Season 12", seasonLabel(12))
        assertEquals("Season 12, episode 33", episodeNumbersLabel(12, 33))
        assertEquals("Season 12", episodeNumbersLabel(12, null))
        assertEquals("Episode 33", episodeNumbersLabel(null, 33))
        assertNull(episodeNumbersLabel(null, null))
    }
}
