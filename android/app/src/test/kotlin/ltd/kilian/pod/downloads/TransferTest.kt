package ltd.kilian.pod.downloads

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.UnknownHostException
import java.nio.file.Files
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import ltd.kilian.pod.net.NetworkState
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.asResponseBody
import okio.buffer
import okio.source
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * One transfer against a stub audio server (an OkHttp interceptor, so no socket is bound):
 * resuming, restarting, the error classes of spec 7.2, cancellation and lost ownership.
 */
@RunWith(RobolectricTestRunner::class)
class TransferTest {
    private val body = ByteArray(300_000) { (it * 31 % 251).toByte() }

    /**
     * Serves `/audio` with an ETag and honours `Range` with a matching `If-Range`; `/status/<n>`
     * answers with that status, `/html` with a page. [stallAt] makes the body stop after that many
     * bytes until the call is cancelled.
     */
    private inner class Stub : Interceptor {
        @Volatile var etag = "\"v1\""
        @Volatile var stallAt: Int? = null
        @Volatile var unreachable = false
        val ranges = mutableListOf<String?>()

        override fun intercept(chain: Interceptor.Chain): Response {
            if (unreachable) throw UnknownHostException("audio.example")
            val request = chain.request()
            val path = request.url.encodedPath
            val response = Response.Builder().request(request).protocol(Protocol.HTTP_1_1).message("")
            fun bytes(from: Int, type: String = "audio/mpeg"): okhttp3.ResponseBody {
                val stall = stallAt
                val stream = object : InputStream() {
                    var position = from

                    override fun read(): Int = throw UnsupportedOperationException()

                    override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
                        if (stall != null && position >= stall) {
                            // Bounded, so a transfer that never cancels its call fails the test
                            // instead of hanging it.
                            val giveUp = System.nanoTime() + 10_000_000_000
                            while (!chain.call().isCanceled() && System.nanoTime() < giveUp) Thread.sleep(5)
                            throw IOException(if (chain.call().isCanceled()) "Canceled" else "stalled")
                        }
                        if (position >= body.size) return -1
                        val end = minOf(body.size, position + length, stall ?: body.size).coerceAtLeast(position + 1)
                        System.arraycopy(body, position, buffer, offset, end - position)
                        return (end - position).also { position = end }
                    }
                }
                return stream.source().buffer().asResponseBody(type.toMediaType(), (body.size - from).toLong())
            }
            return when {
                path == "/audio" -> {
                    val range = request.header("Range")
                    synchronized(ranges) { ranges += range }
                    val start = range?.removePrefix("bytes=")?.removeSuffix("-")?.toInt()
                    if (start != null && request.header("If-Range") == etag) {
                        if (start >= body.size) {
                            response.code(416).header("Content-Range", "bytes */${body.size}").body(bytes(body.size)).build()
                        } else {
                            response.code(206).header("ETag", etag).header("Content-Range", "bytes $start-${body.size - 1}/${body.size}").body(bytes(start)).build()
                        }
                    } else {
                        response.code(200).header("ETag", etag).body(bytes(0)).build()
                    }
                }
                path == "/html" -> response.code(200).body(bytes(0, "text/html; charset=utf-8")).build()
                path.startsWith("/status/") -> response.code(path.removePrefix("/status/").toInt()).body(bytes(body.size)).build()
                else -> error("unexpected $path")
            }
        }
    }

    private val stub = Stub()
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java).build()
    private val db get() = database.openHelper.writableDatabase
    private val root: File = Files.createTempDirectory("downloads").toFile()
    private var network = NetworkState(connected = true, metered = false)
    private val transfer = Transfer(database, OkHttpClient.Builder().addInterceptor(stub).build(), root, { network }, checkpointEveryMs = 0)

    @After
    fun close() {
        database.close()
        root.deleteRecursively()
    }

    private val episode = "e0000000-0000-5000-8000-000000000001"
    private val podcast = "a1000000-0000-5000-8000-000000000001"
    private val run = "0199b000-0000-7000-8000-000000000001"

    /** An episode and its `transferring` row under [run], fetching [path]. */
    private fun seed(path: String = "/audio", bytesDone: Long = 0, validator: String? = null, attempts: Int = 0) {
        db.exec(
            """insert into episode (id, podcast_id, title, summary_html, content_html, published_at, audio_url, audio_type, version)
               values (?, ?, 'The pager', '', '', 0, ?, 'audio/mpeg', 1)""",
            episode, podcast, "https://audio.example$path",
        )
        db.exec(
            "insert into download (episode_id, state, audio_url, path, run_id, bytes_done, validator, attempts) values (?, 'transferring', ?, '', ?, ?, ?, ?)",
            episode, "https://audio.example$path", run, bytesDone, validator, attempts,
        )
    }

    private data class RowState(val state: String, val path: String, val bytesDone: Long, val attempts: Long, val waitReason: String?, val nextAttemptAt: Long?)

    private fun row(): RowState? = db.queryOne("select * from download where episode_id = ?", episode) {
        RowState(it.string("state"), it.string("path"), it.long("bytes_done"), it.long("attempts"), it.stringOrNull("wait_reason"), it.longOrNull("next_attempt_at"))
    }

    private val files get() = RunFiles.of(root, podcast, episode, run, "audio/mpeg")!!

    @Test
    fun aCompleteBodyIsPublishedUnderTheRunsName() = runBlocking {
        seed()
        val seen = mutableListOf<Progress>()
        assertEquals(Outcome.Complete, transfer.run(episode, run) { seen += it })
        val row = row()!!
        assertEquals("complete", row.state)
        assertEquals("$podcast/$episode.$run.mp3", row.path)
        assertArrayEquals(body, File(root, row.path).readBytes())
        assertFalse(files.part.exists())
        assertEquals(body.size.toLong(), db.queryOne("select bytes from known_size") { it.long("bytes") })
        assertEquals(Progress(body.size.toLong(), body.size.toLong()), seen.last())
    }

    @Test
    fun aResumeTruncatesToTheCheckpointAndAsksForTheRest() = runBlocking {
        seed(bytesDone = 100_000, validator = "\"v1\"")
        // Past the checkpoint the `.part` holds a torn tail, which must not be kept.
        files.part.parentFile!!.mkdirs()
        files.part.writeBytes(body.copyOf(100_000) + ByteArray(5_000) { 7 })
        assertEquals(Outcome.Complete, transfer.run(episode, run))
        assertEquals(listOf<String?>("bytes=100000-"), stub.ranges)
        assertArrayEquals(body, files.finished.readBytes())
    }

    @Test
    fun aChangedEtagRestartsFromZero() = runBlocking {
        seed(bytesDone = 100_000, validator = "\"v0\"")
        files.part.parentFile!!.mkdirs()
        files.part.writeBytes(ByteArray(100_000) { 1 })
        assertEquals(Outcome.Complete, transfer.run(episode, run))
        assertArrayEquals(body, files.finished.readBytes())
    }

    @Test
    fun a416WithAFullPartCountsAsDone() = runBlocking {
        seed(bytesDone = body.size.toLong(), validator = "\"v1\"")
        files.part.parentFile!!.mkdirs()
        files.part.writeBytes(body)
        assertEquals(Outcome.Complete, transfer.run(episode, run))
        assertArrayEquals(body, files.finished.readBytes())
    }

    @Test
    fun a404EndsInFailed() = runBlocking {
        seed("/status/404")
        assertEquals(Outcome.Failed, transfer.run(episode, run))
        assertEquals("failed", row()!!.state)
        assertFalse(files.part.exists())
    }

    @Test
    fun anHtmlResponseIsPermanent() = runBlocking {
        seed("/html")
        assertEquals(Outcome.Failed, transfer.run(episode, run))
    }

    @Test
    fun aServerErrorWaitsForARetryWithBackoff() = runBlocking {
        seed("/status/503")
        val before = System.currentTimeMillis()
        assertEquals(Outcome.WaitingRetry(1), transfer.run(episode, run))
        val row = row()!!
        assertEquals("waiting_retry", row.state)
        assertEquals(1, row.attempts)
        assertTrue("${row.nextAttemptAt}", row.nextAttemptAt!! - before in 60_000..61_000)
    }

    @Test
    fun theEighthAttemptFailsForGood() = runBlocking {
        seed("/status/503", attempts = 7)
        assertEquals(Outcome.Failed, transfer.run(episode, run))
        assertEquals(8, row()!!.attempts)
    }

    @Test
    fun aConnectErrorRequeuesWithoutUsingARetry() = runBlocking {
        seed()
        stub.unreachable = true
        assertEquals(Outcome.Queued(WaitReason.Connection), transfer.run(episode, run))
        val row = row()!!
        assertEquals("queued" to "connection", row.state to row.waitReason)
        assertEquals(0, row.attempts)
    }

    @Test
    fun aCancelledTransferCheckpointsFirst() = runBlocking {
        seed()
        stub.stallAt = 120_000
        val running = async { transfer.run(episode, run) }
        withTimeout(5_000) { while ((row()?.bytesDone ?: 0) < 120_000) yield() }
        running.cancel()
        withTimeout(5_000) { running.join() }
        val row = row()!!
        assertEquals("transferring", row.state)
        assertEquals(120_000, row.bytesDone)
        assertEquals(120_000, files.part.length())
    }

    @Test
    fun aDeletedRowMakesTheTransferDeleteItsPart() = runBlocking {
        seed()
        stub.stallAt = 50_000
        val running = async { transfer.run(episode, run) }
        withTimeout(5_000) { while (!files.part.exists() || files.part.length() < 50_000) yield() }
        db.exec("delete from download")
        // The stalled read only ends when the call does; a dropped connection is the usual way.
        stub.stallAt = null
        running.cancel()
        withTimeout(5_000) { running.join() }
        assertNull(row())
        assertFalse(files.part.exists())
    }

    @Test
    fun retriesBackOffUpToSixHours() {
        assertEquals(60_000, retryDelayMs(1))
        assertEquals(6 * 3_600_000L, retryDelayMs(12))
        assertEquals(6 * 3_600_000L, retryDelayMs(64))
    }

    @Test
    fun fileNamesStayInsideTheRoot() {
        assertNull(RunFiles.of(root, "../etc", episode, run, "audio/mpeg"))
        assertNull(RunFiles.of(root, podcast, "a/b", run, "audio/mpeg"))
        assertEquals("$podcast/$episode.$run.m4a", RunFiles.of(root, podcast, episode, run, "audio/x-m4a; codecs=aac")!!.relative)
    }
}
