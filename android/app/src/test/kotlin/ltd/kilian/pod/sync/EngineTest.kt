package ltd.kilian.pod.sync

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import ltd.kilian.pod.core.FeedUrl
import ltd.kilian.pod.core.FeedUrlResult
import ltd.kilian.pod.core.MutationResult
import ltd.kilian.pod.core.MutationStatus
import ltd.kilian.pod.core.PodcastScope
import ltd.kilian.pod.core.ScopeChanges
import ltd.kilian.pod.core.SyncRequest
import ltd.kilian.pod.core.SyncResponse
import ltd.kilian.pod.core.UserScope
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.JSON
import ltd.kilian.pod.net.NetworkState
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.queryOne
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The sync loop's request failures and its mutex, against a scripted stub in place of the
 * network (an OkHttp interceptor, so no socket is bound). The store steps themselves are the
 * conformance fixtures' job ([SyncConformanceTest]).
 */
@RunWith(RobolectricTestRunner::class)
class EngineTest {
    /**
     * How the stub answers. A pull-only request gets [pull], one carrying mutations gets [push].
     * `200` is an empty, complete page that applies every mutation.
     */
    private class Stub : Interceptor {
        @Volatile var push = 200
        @Volatile var pull = 200
        @Volatile var retryAfter: Long? = null

        /** Leave the podcast scopes out of the answer, which no server does. */
        @Volatile var dropPodcastScopes = false

        /** Never answer, until the call is cancelled. */
        @Volatile var hang = false
        val requests = AtomicInteger()

        override fun intercept(chain: Interceptor.Chain): Response {
            requests.incrementAndGet()
            while (hang) {
                if (chain.call().isCanceled()) throw java.io.IOException("Canceled")
                Thread.sleep(5)
            }
            val body = Buffer().also { chain.request().body!!.writeTo(it) }.readUtf8()
            val request = WireJson.decodeFromString(SyncRequest.serializer(), body)
            val status = if (request.mutations.isEmpty()) pull else push
            val respond = { code: Int, json: String ->
                Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(code).message("")
                    .apply { retryAfter?.let { header("Retry-After", it.toString()) } }
                    .body(json.toResponseBody(JSON)).build()
            }
            if (status != 200) return respond(status, "{}")
            val cursors = request.scopes!!
            val response = SyncResponse(
                serverTime = System.currentTimeMillis(),
                results = request.mutations.map { MutationResult(it.id, MutationStatus.Applied, at = it.at) },
                scopes = ScopeChanges(
                    user = UserScope(cursors.user, complete = true, subscriptions = emptyList(), episodeStates = emptyList(), settings = emptyList()),
                    podcasts = if (dropPodcastScopes) emptyList() else cursors.podcasts.map { PodcastScope.Changes(it.id, it.version, complete = true) },
                ),
            )
            return respond(200, WireJson.encodeToString(SyncResponse.serializer(), response))
        }
    }

    private val stub = Stub()
    private val network = MutableStateFlow(NetworkState(connected = true, metered = false))
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java).build()
    private val handle = SyncHandle()
    private val engine = SyncEngine(database, handle, OkHttpClient.Builder().addInterceptor(stub).build(), network)

    private fun count(sql: String): Long = database.openHelper.writableDatabase.queryOne(sql) { it.long("n") }!!

    private fun subscribeChange(): LocalChange {
        val url = (FeedUrl.normalize("https://example.com/feed.xml") as FeedUrlResult.Ok).url
        return LocalChange.Subscription(url.podcastId, url.feedUrl, subscribed = true)
    }

    /** A signed-in store with one queued mutation. */
    @Before
    fun signIn() = runBlocking {
        database.openHelper.writableDatabase.exec(
            """insert into account (id, server_url, user_id, username, device_id, token)
               values (1, 'https://pod.example', '0192f0a1-0000-7000-8000-000000000001', 'alice',
                       '0192f0a1-0000-7000-8000-000000000002', 'token')""",
        )
        writeLocal(database, handle, subscribeChange())
        Unit
    }

    @After
    fun close() = database.close()

    @Test
    fun aRoundTripPushesTheOutboxAndPullsEveryScope() = runBlocking {
        val state = engine.sync()
        assertTrue("$state", state is EngineState.Idle && state.lastSynced != null)
        assertEquals(state, engine.state.value)
        assertEquals(0, count("select count(*) as n from outbox"))
        // The push, then the new podcast scope's first page.
        assertEquals(2, stub.requests.get())
        assertEquals(0, count("select count(*) as n from sync_scope where complete = 0"))
    }

    @Test
    fun unauthorizedSignsOutAndKeepsTheOutbox() = runBlocking {
        stub.push = 401
        assertEquals(EngineState.SignedOut, engine.sync())
        assertEquals(EngineState.SignedOut, engine.state.value)
        assertEquals(1, count("select count(*) as n from outbox"))

        // The failed batch is no longer in flight, so a new write replaces it.
        val mutation = writeLocal(database, handle, subscribeChange())
        assertEquals(1, count("select count(*) as n from outbox"))
        assertEquals(1, count("select count(*) as n from outbox where mutation_id = '${mutation.id}'"))
    }

    @Test
    fun aBatchRefusedFiveTimesMovesToTheRejectedLog() = runBlocking {
        stub.push = 400
        for (attempt in 1L..4L) {
            // Each loop counts the attempt, then finishes pull-only.
            val state = engine.sync()
            assertTrue("$state", state is EngineState.Idle && state.lastSynced != null)
            assertEquals("after attempt $attempt", attempt, count("select attempts as n from outbox"))
        }
        engine.sync()
        assertEquals(0, count("select count(*) as n from outbox"))
        assertEquals(1, count("select count(*) as n from rejected_mutation where reason = 'bad_request'"))
    }

    @Test
    fun serverErrorsBackOffDoublingUntilARequestSucceeds() = runBlocking {
        stub.push = 503
        fun backoff(state: EngineState): Long = (state as? EngineState.BackingOff ?: error("expected a backoff, got $state")).retryAt - monotonicMs()
        val first = backoff(engine.sync())
        assertTrue("$first", first in 4_001..5_000)
        val second = backoff(engine.sync())
        assertTrue("$second", second in 9_001..10_000)
        assertEquals(1, count("select count(*) as n from outbox"))

        stub.push = 200
        val ended = async { engine.loopEnded.first() }
        yield()
        assertTrue(engine.sync() is EngineState.Idle)
        assertEquals(0, count("select count(*) as n from outbox"))
        withTimeout(1_000) { ended.await() }

        // A success resets the backoff, and `429` waits at least `Retry-After`.
        stub.pull = 429
        stub.retryAfter = 60
        val limited = backoff(engine.sync())
        assertTrue("$limited", limited in 59_001..60_000)
    }

    @Test
    fun aResponseThatMovesNothingEndsTheLoop() = runBlocking {
        stub.dropPodcastScopes = true
        val state = withTimeout(10_000) { engine.sync() }
        assertTrue("$state", state is EngineState.Idle)
        assertEquals(0, count("select count(*) as n from outbox"))
    }

    @Test
    fun offlineEndsTheLoopWithoutARequest() = runBlocking {
        network.value = NetworkState.OFFLINE
        assertEquals(EngineState.Offline, engine.sync())
        assertEquals(0, stub.requests.get())
    }

    @Test
    fun aPauseCancelsTheRequestInFlightAndHoldsTheMutex() = runBlocking {
        stub.hang = true
        val loop = async { engine.sync() }
        withTimeout(5_000) { while (stub.requests.get() == 0) yield() }
        val pause = withTimeout(5_000) { engine.pause() }
        assertEquals(EngineState.Idle(null), withTimeout(5_000) { loop.await() })
        assertEquals("nothing was applied", 1, count("select count(*) as n from outbox"))

        stub.hang = false
        val next = async { engine.sync() }
        yield()
        assertTrue("no loop runs while paused", !next.isCompleted)
        assertEquals(1, stub.requests.get())
        pause.close()
        assertTrue(withTimeout(5_000) { next.await() } is EngineState.Idle)
        assertEquals(0, count("select count(*) as n from outbox"))
    }

    @Test
    fun aWriteDuringARequestGoesOutInTheSameLoop() = runBlocking {
        stub.hang = true
        val loop = async { engine.sync() }
        withTimeout(5_000) { while (stub.requests.get() == 0) yield() }
        writeLocal(database, handle, LocalChange.Setting("inbox_window_days", kotlinx.serialization.json.JsonPrimitive(7)))
        stub.hang = false
        assertTrue(withTimeout(5_000) { loop.await() } is EngineState.Idle)
        assertEquals("the write made during the request was sent too", 0, count("select count(*) as n from outbox"))
    }

    @Test
    fun aWaitingShowResyncsOnTheScheduleUntilItIsFetched() {
        val resyncs = FirstFetchResyncs()
        assertEquals("nothing waits", null, resyncs.next(emptySet()))
        assertEquals("after 30 s the timer takes over", listOf(2_000L, 5_000L, 15_000L, 30_000L, null, null), (0 until 6).map { resyncs.next(setOf("a")) })
        assertEquals("a new show starts over", 2_000L, resyncs.next(setOf("a", "b")))
        assertEquals(5_000L, resyncs.next(setOf("a", "b")))
        assertEquals("b's fetch doesn't restart the schedule", 15_000L, resyncs.next(setOf("a")))
        assertEquals("every show is fetched", null, resyncs.next(emptySet()))
        assertEquals("a later subscribe gets the whole schedule", 2_000L, resyncs.next(setOf("a")))
    }
}
