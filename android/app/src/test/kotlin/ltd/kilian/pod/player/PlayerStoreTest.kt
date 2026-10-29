package ltd.kilian.pod.player

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import java.io.File
import java.nio.file.Files
import kotlinx.coroutines.runBlocking
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.sync.SyncHandle
import ltd.kilian.pod.sync.writeLocal
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/** The store side of the playback contract: what the player loads, and what its writes leave behind. */
@RunWith(RobolectricTestRunner::class)
class PlayerStoreTest {
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java).build()
    private val db get() = database.openHelper.writableDatabase
    private val root: File = Files.createTempDirectory("downloads").toFile()
    private val sync = SyncHandle()

    @Before
    fun seed() {
        db.exec(
            """insert into account (id, server_url, user_id, username, device_id, token)
               values (1, 'https://pod.example', 'user', 'alice', 'device', 'token')""",
        )
        db.exec("insert into podcast (id, feed_url, status, version, title) values ('p1', 'https://example.com/feed', 'ok', 1, 'Darknet Diaries')")
        db.exec(
            """insert into episode (id, podcast_id, title, summary_html, content_html, published_at, duration_ms,
                                    audio_url, audio_type, version)
               values ('e1', 'p1', 'EP 151: The pager', '', '', 0, 3120000, 'https://example.com/e1.mp3', 'audio/mpeg', 1)""",
        )
    }

    @After
    fun close() {
        database.close()
        root.deleteRecursively()
    }

    @Test
    fun anEpisodeWithoutStateStartsAtTheBeginningFromTheStream() {
        val episode = loadEpisode(db, "e1", root)!!
        assertEquals("Darknet Diaries", episode.podcastTitle)
        assertEquals(Stored(0, played = false), episode.stored)
        assertEquals(Source.Stream("https://example.com/e1.mp3"), episode.source)
        assertNull(loadEpisode(db, "missing", root))
    }

    @Test
    fun aCompleteDownloadPlaysOnlyWhileItsFileExists() {
        db.exec("insert into download (episode_id, state, audio_url, path) values ('e1', 'complete', 'https://example.com/e1.mp3', 'e1.mp3')")
        assertEquals("the row alone isn't enough", Source.Stream("https://example.com/e1.mp3"), loadEpisode(db, "e1", root)!!.source)
        File(root, "e1.mp3").writeText("audio")
        assertEquals(Source.Download(File(root, "e1.mp3")), loadEpisode(db, "e1", root)!!.source)

        db.exec("update download set state = 'downloading'")
        assertEquals("a partial file streams", Source.Stream("https://example.com/e1.mp3"), loadEpisode(db, "e1", root)!!.source)
    }

    @Test
    fun theRememberedEpisodeIsRestoredAndAMissingOneDropped() {
        assertNull(loadLastEpisode(db, root))
        rememberEpisode(db, "e1")
        rememberEpisode(db, "e1")
        assertEquals("e1", loadLastEpisode(db, root)?.id)
        rememberEpisode(db, "gone")
        assertNull(loadLastEpisode(db, root))
    }

    @Test
    fun replayingAPlayedEpisodeWritesTheReopenAsItsOwnMutation() = runBlocking {
        db.exec("insert into episode_state (episode_id, podcast_id, position_ms, played) values ('e1', 'p1', 3120000, 1)")
        val episode = loadEpisode(db, "e1", root)!!
        val (contract, start) = Contract.start(episode.stored, episode.durationMs, 30_000, 0)
        writeLocal(database, sync, changesFor(episode, start))
        writeLocal(database, sync, changesFor(episode, contract.progress(5_000, 5_000)))
        writeLocal(database, sync, changesFor(episode, contract.progress(10_000, 10_000)))

        val state = db.queryOne("select position_ms, played from episode_state where episode_id = 'e1'") { it.long("position_ms") to it.flag("played") }
        assertEquals(10_000L to false, state)
        // The position saves coalesce into one row; the reopen keeps its own.
        assertEquals(
            listOf("played:e1", "position:e1"),
            db.queryList("select coalesce_key from outbox order by created_at, coalesce_key") { it.string("coalesce_key") },
        )
    }

    @Test
    fun crossingTheThresholdMarksTheEpisodePlayed() = runBlocking {
        val episode = loadEpisode(db, "e1", root)!!
        val (contract, _) = Contract.start(episode.stored, episode.durationMs, 30_000, 0)
        writeLocal(database, sync, changesFor(episode, contract.seek(episode.durationMs!! - 10_000, 1_000)))
        assertEquals(true, db.queryOne("select played from episode_state where episode_id = 'e1'") { it.flag("played") })
    }
}
