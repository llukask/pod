package ltd.kilian.pod.notify

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import ltd.kilian.pod.core.Episode
import ltd.kilian.pod.core.PodcastScope
import ltd.kilian.pod.core.ScopeChanges
import ltd.kilian.pod.core.SyncResponse
import ltd.kilian.pod.core.UserScope
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.sync.applyResponse
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class NewEpisodesTest {
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java)
        .allowMainThreadQueries().build()
    private val db get() = database.openHelper.writableDatabase
    private val now = 1_800_000_000_000L
    private val loud = "11111111-1111-5111-8111-111111111111"
    private val quiet = "22222222-2222-5222-8222-222222222222"

    @After
    fun close() = database.close()

    private fun episode(id: String, podcast: String, ageHours: Long) = Episode(
        id = id, podcastId = podcast, title = "Episode $id", summaryHtml = "", contentHtml = "",
        publishedAt = now - ageHours * 3_600_000, audioUrl = "https://example.com/$id.mp3", audioType = "audio/mpeg", version = 1,
    )

    private fun page(podcast: String, vararg episodes: Episode) = PodcastScope.Changes(podcast, 1, complete = true, episodes = episodes.toList())

    @Test
    fun onlyNewRecentEpisodesOfNotifyingShowsAreNamed() {
        db.exec("insert into account (id, server_url, user_id, username, device_id, token) values (1, 'https://p', 'u', 'a', 'd', 't')")
        for ((id, title) in listOf(loud to "Loud", quiet to "Quiet")) {
            db.exec("insert into podcast (id, feed_url, status, title, version) values (?, 'https://example.com/f', 'ok', ?, 1)", id, title)
            db.exec("insert into subscription (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev) values (?, 'https://example.com/f', 1, 0, 'd')", id)
            db.exec("insert into sync_scope (scope, version, complete) values (?, 0, 0)", "podcast:$id")
        }
        db.exec("insert into podcast_device (podcast_id, notify) values (?, 1)", loud)
        // Already known before this sync: an update, not an insert.
        db.exec(
            """insert into episode (id, podcast_id, title, summary_html, content_html, published_at, audio_url, audio_type, version)
               values ('old', ?, 'Old', '', '', ?, 'https://example.com/old.mp3', 'audio/mpeg', 1)""",
            loud, now,
        )

        val inserted = mutableSetOf<String>()
        val response = SyncResponse(
            now, emptyList(),
            ScopeChanges(
                UserScope(1, true, emptyList(), emptyList(), emptyList()),
                listOf(
                    page(loud, episode("old", loud, 0), episode("fresh", loud, 1), episode("stale", loud, 49)),
                    page(quiet, episode("muted", quiet, 1)),
                ),
            ),
        )
        applyResponse(db, "u", response, now, inserted)

        assertEquals(setOf("fresh", "stale", "muted"), inserted)
        val named = episodesToNotify(db, inserted, now)
        assertEquals(listOf(NewEpisode("Loud", "Episode fresh")), named)
        assertEquals("Loud", notificationTitle(named))
        assertEquals("2 new episodes", notificationTitle(named + named))
    }
}
