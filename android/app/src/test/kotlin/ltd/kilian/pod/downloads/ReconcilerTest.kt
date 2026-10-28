package ltd.kilian.pod.downloads

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.nio.file.Files
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import ltd.kilian.pod.net.NetworkState
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.sync.SyncHandle
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.asResponseBody
import okio.buffer
import okio.source
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The reconciler's executor around the plan: starting and cancelling jobs, the "Archived and
 * removed" path, logout's stop and the startup repair. Jobs run as coroutines in place of
 * WorkManager, and audio comes from an interceptor.
 */
@RunWith(RobolectricTestRunner::class)
class ReconcilerTest {
    private val body = ByteArray(200_000) { (it % 199).toByte() }

    /** Serves [body] for every URL; with [stall] set the body stops halfway until the call is cancelled. */
    private inner class Audio : Interceptor {
        @Volatile var stall = false

        override fun intercept(chain: Interceptor.Chain): Response {
            val stalling = stall
            val stream = object : InputStream() {
                var position = 0

                override fun read(): Int = throw UnsupportedOperationException()

                override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
                    if (stalling && position >= body.size / 2) {
                        val giveUp = System.nanoTime() + 10_000_000_000
                        while (!chain.call().isCanceled() && System.nanoTime() < giveUp) Thread.sleep(5)
                        throw IOException("Canceled")
                    }
                    if (position >= body.size) return -1
                    val end = minOf(body.size, position + length, if (stalling) body.size / 2 else body.size)
                    System.arraycopy(body, position, buffer, offset, end - position)
                    return (end - position).also { position = end }
                }
            }
            return Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(200).message("")
                .header("ETag", "\"v1\"")
                .body(stream.source().buffer().asResponseBody("audio/mpeg".toMediaType(), body.size.toLong()))
                .build()
        }
    }

    /** [TransferJobs] as coroutines on [scope]. */
    private inner class CoroutineJobs : TransferJobs {
        val jobs = ConcurrentHashMap<String, Job>()
        val expedited = ConcurrentHashMap.newKeySet<String>()

        override fun start(episodeId: String, expedited: Boolean) {
            if (expedited) this.expedited += episodeId
            jobs[episodeId] = scope.launch { reconciler.runTransfer(episodeId) }
        }

        override fun cancel(episodeId: String) {
            jobs.remove(episodeId)?.cancel()
        }

        override fun cancelAll() {
            jobs.values.forEach { it.cancel() }
            jobs.clear()
        }

        override suspend fun live(): Set<String> = jobs.filterValues { it.isActive }.keys
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val audio = Audio()
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java).build()
    private val db get() = database.openHelper.writableDatabase
    private val root: File = Files.createTempDirectory("downloads").toFile()
    private val network = MutableStateFlow(NetworkState(connected = true, metered = false))
    private val jobs = CoroutineJobs()
    private val sync = SyncHandle()
    private val reconciler = Reconciler(
        database,
        Transfer(database, OkHttpClient.Builder().addInterceptor(audio).build(), root, { network.value }),
        network,
        jobs,
        freeSpace = { 100L shl 30 },
    )

    private val podcast = "a1000000-0000-5000-8000-000000000001"
    private fun episode(n: Int) = "e0000000-0000-5000-8000-%012d".format(n)

    @Before
    fun seed() {
        db.exec(
            "insert into account (id, server_url, user_id, username, device_id, token) values (1, 'https://pod.example', 'u', 'alice', '0192f0a1-0000-7000-8000-000000000002', 't')",
        )
        for (n in 1..3) {
            db.exec(
                """insert into episode (id, podcast_id, title, summary_html, content_html, published_at, audio_url, audio_type, audio_bytes, version)
                   values (?, ?, ?, '', '', ?, ?, 'audio/mpeg', ?, 1)""",
                episode(n), podcast, "Episode $n", n * 1_000L, "https://audio.example/$n.mp3", body.size,
            )
        }
    }

    @After
    fun close() {
        scope.cancel()
        database.close()
        root.deleteRecursively()
    }

    private fun state(n: Int): String? = db.queryOne("select state from download where episode_id = ?", episode(n)) { it.string("state") }

    private suspend fun waitFor(what: String, check: () -> Boolean) = withTimeout(10_000) {
        while (!check()) yield()
    }.also { assertTrue(what, check()) }

    @Test
    fun aPinIsFetchedAsAnExpeditedJobAndThenLeftAlone() = runBlocking {
        pin(database, episode(1), meteredOk = false)
        reconciler.pass()
        assertEquals(setOf(episode(1)), jobs.expedited)
        waitFor("the download completes") { state(1) == "complete" }
        val path = db.queryOne("select path from download") { it.string("path") }!!
        assertEquals(body.size.toLong(), File(root, path).length())
        waitFor("the job ends") { jobs.jobs.values.none { it.isActive } }
        assertTrue("progress is in memory only while running", reconciler.progress.value.isEmpty())

        reconciler.pass()
        assertEquals("a second pass starts nothing", 1, jobs.jobs.size)
    }

    @Test
    fun cancellingAnAutoDownloadArchivesItAndUndoBringsItBack() = runBlocking {
        db.exec("insert into podcast_device (podcast_id, auto_keep) values (?, 1)", podcast)
        audio.stall = true
        reconciler.pass()
        waitFor("the newest episode downloads") { reconciler.progress.value.containsKey(episode(3)) }

        val removed = perform(database, sync, reconciler, episode(3), podcast, Act.Cancel)
        assertNotNull("an auto download is archived", removed)
        assertTrue(db.queryOne("select archived from episode_state where episode_id = ?", episode(3)) { it.flag("archived") }!!)
        assertEquals(1L, db.queryOne("select count(*) as n from outbox") { it.long("n") })

        reconciler.pass()
        waitFor("the transfer stops") { !jobs.jobs.containsKey(episode(3)) && episode(3) !in reconciler.progress.value }
        val wishes = db.queryList("select episode_id from download_wish") { it.string("episode_id") }
        assertEquals("the rule moves on to the next unplayed episode", listOf(episode(2)), wishes)
        assertEquals(null, state(3))
        assertTrue("no file is left", root.walk().none { it.isFile && it.name.startsWith(episode(3)) })

        audio.stall = false
        undo(database, sync, reconciler, removed!!)
        assertFalse(db.queryOne("select archived from episode_state where episode_id = ?", episode(3)) { it.flag("archived") }!!)
        reconciler.pass()
        assertEquals(listOf(episode(3)), db.queryList("select episode_id from download_wish where source = 'auto'") { it.string("episode_id") })
    }

    @Test
    fun stoppingCancelsTransfersAndHoldsOffPasses() = runBlocking {
        audio.stall = true
        pin(database, episode(1), meteredOk = false)
        reconciler.pass()
        waitFor("the transfer runs") { reconciler.progress.value.isNotEmpty() }

        reconciler.stopped(timeoutMs = 10_000) {
            assertTrue("every transfer stopped", jobs.jobs.values.none { it.isActive })
            val pass = async { reconciler.pass() }
            yield()
            assertFalse("no pass runs while stopped", pass.isCompleted)
            db.exec("delete from download")
            db.exec("delete from download_wish")
            pass.cancel()
        }
        reconciler.pass()
        assertEquals(null, state(1))
    }

    @Test
    fun startupRepairPublishesDropsAndSweeps() = runBlocking {
        val run = "0199b000-0000-7000-8000-000000000001"
        val finished = RunFiles.of(root, podcast, episode(1), run, "audio/mpeg")!!
        finished.finished.parentFile!!.mkdirs()
        finished.finished.writeBytes(body)
        // A crash between the rename and the row write.
        db.exec("insert into download (episode_id, state, audio_url, path, run_id, bytes_total) values (?, 'transferring', 'u', '', ?, ?)", episode(1), run, body.size)
        // A complete row whose file is gone.
        db.exec("insert into download (episode_id, state, audio_url, path, run_id) values (?, 'complete', 'u', ?, 'r2')", episode(2), "$podcast/gone.mp3")
        val stray = File(File(root, podcast), "stray.mp3.part").apply { writeText("x") }

        reconciler.repair()
        assertEquals("complete", state(1))
        assertEquals("${podcast}/${finished.finished.name}", db.queryOne("select path from download where episode_id = ?", episode(1)) { it.string("path") })
        assertEquals(null, state(2))
        assertFalse(stray.exists())
        assertTrue(finished.finished.exists())
    }
}
