package ltd.kilian.pod.sonos

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import ltd.kilian.pod.core.ErrorBody
import ltd.kilian.pod.core.ErrorDetail
import ltd.kilian.pod.core.SonosCommand
import ltd.kilian.pod.core.SonosPlayRequest
import ltd.kilian.pod.core.SonosPlayer
import ltd.kilian.pod.core.SonosPlayerState
import ltd.kilian.pod.core.SonosPlayersResponse
import ltd.kilian.pod.core.SonosReleased
import ltd.kilian.pod.core.SonosSessionView
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.JSON
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The Sonos controller's remote mode against a scripted pod-server (an OkHttp interceptor, so no
 * socket is bound and no speaker is involved). The pure poll decisions are `core`'s `SonosTest`.
 */
@RunWith(RobolectricTestRunner::class)
class SonosControllerTest {
    private val episode = "0192f0a0-0000-7000-8000-000000000001"
    private val couch = SonosTarget("RINCON_804AF246458401400", "Couch")

    /** A household of one group, and how its endpoints answer. */
    private class Server : Interceptor {
        @Volatile var players: List<SonosPlayer> = emptyList()

        /** The status `play`, `command` and `release` answer with; 200 runs them. */
        @Volatile var failWith: Pair<Int, String>? = null
        @Volatile var released = SonosReleased("", 0, false)

        /** Holds the next play request until counted down. */
        @Volatile var playGate: CountDownLatch? = null

        /** Holds the next volume command until counted down. */
        @Volatile var volumeGate: CountDownLatch? = null
        val volumeArrived = CountDownLatch(1)

        /** Every request as "METHOD /path body", and the flushes the controller made, in order. */
        val log: MutableList<String> = Collections.synchronizedList(mutableListOf())

        override fun intercept(chain: Interceptor.Chain): Response {
            val request = chain.request()
            val path = request.url.encodedPath
            val body = request.body?.let { b -> Buffer().also { b.writeTo(it) }.readUtf8() }.orEmpty()
            log += "${request.method} $path $body".trim()
            val respond = { code: Int, json: String ->
                Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(code).message("").body(json.toResponseBody(JSON)).build()
            }
            if (path == "/v1/sonos/players") return respond(200, WireJson.encodeToString(SonosPlayersResponse.serializer(), SonosPlayersResponse(players)))
            if (path.endsWith("/play")) playGate?.await(5, TimeUnit.SECONDS)
            if (path.endsWith("/command") && "\"volume\"" in body) {
                volumeArrived.countDown()
                volumeGate?.let { gate ->
                    volumeGate = null
                    gate.await(5, TimeUnit.SECONDS)
                }
            }
            failWith?.let { (status, code) -> return respond(status, WireJson.encodeToString(ErrorBody.serializer(), ErrorBody(ErrorDetail(code, "refused")))) }
            // Pause and resume act on the group, so the answer shows them.
            val playing = when {
                "\"pause\"" in body -> false
                "\"resume\"" in body -> true
                else -> null
            }
            if (playing != null) players = players.map { it.copy(session = it.session?.copy(playing = playing)) }
            return when {
                path.endsWith("/release") -> respond(200, WireJson.encodeToString(SonosReleased.serializer(), released))
                else -> respond(200, WireJson.encodeToString(SonosPlayer.serializer(), players.single()))
            }
        }
    }

    private val server = Server()
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java).build()
    private val foreground = MutableStateFlow(true)
    private val syncs = AtomicInteger()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val sonos = SonosController(
        SonosApi(database, OkHttpClient.Builder().addInterceptor(server).build()),
        flushWrites = { server.log += "flush" },
        requestSync = { syncs.incrementAndGet() },
        foreground = foreground,
        scope = scope,
    )

    private fun playing(positionMs: Long = 90_000, playing: Boolean = true, volume: Int = 12) =
        SonosPlayer(couch.id, couch.name, volume, SonosPlayerState.Pod, SonosSessionView(episode, playing, positionMs))

    @Before
    fun signIn() {
        database.openHelper.writableDatabase.exec(
            """insert into account (id, server_url, user_id, username, device_id, token)
               values (1, 'https://pod.example', '0192f0a1-0000-7000-8000-000000000001', 'alice',
                       '0192f0a1-0000-7000-8000-000000000002', 'token')""",
        )
    }

    @After
    fun close() {
        scope.cancel()
        database.close()
    }

    private fun waitFor(what: String, timeoutMs: Long = 5_000, condition: () -> Boolean) {
        val until = System.currentTimeMillis() + timeoutMs
        while (!condition()) {
            check(System.currentTimeMillis() < until) { "timed out waiting for $what; state ${sonos.state.value}, log ${server.log}" }
            Thread.sleep(5)
        }
    }

    private fun handedOff() {
        server.players = listOf(playing())
        sonos.start(couch, episode, 90_000)
        waitFor("remote mode") { sonos.state.value.output is Output.Sonos }
        server.log.clear()
    }

    /** The next snackbar message, subscribed before the caller acts so none is missed. */
    private fun CoroutineScope.nextMessage() = async(start = CoroutineStart.UNDISPATCHED) { withTimeout(5_000) { sonos.messages.first() } }

    private fun commandsSent(): List<SonosCommand> = server.log.filter { it.startsWith("POST /v1/sonos/players/${couch.id}/command") }
        .map { WireJson.decodeFromString(SonosCommand.serializer(), it.substringAfter("/command ")) }

    @Test
    fun aHandOffPushesTheOutboxThenPlaysAtTheStartPosition() {
        server.players = listOf(playing(volume = 23))
        val gate = CountDownLatch(1)
        server.playGate = gate
        sonos.start(couch, episode, 90_000)
        waitFor("connecting while the play request is out") { (sonos.state.value.output as? Output.Connecting)?.target == couch }
        assertEquals(false, sonos.state.value.session!!.playing)
        gate.countDown()
        waitFor("remote mode") { sonos.state.value.output is Output.Sonos }
        val play = SonosPlayRequest(episode, 90_000)
        assertEquals(listOf("flush", "POST /v1/sonos/players/${couch.id}/play ${WireJson.encodeToString(SonosPlayRequest.serializer(), play)}"), server.log.take(2))
        assertEquals(23, sonos.state.value.volume)
        assertTrue(sonos.state.value.session!!.playing)
    }

    @Test
    fun aSpeakerThatDoesntAnswerLeavesTheEpisodeHereAndSaysSo() = runBlocking {
        server.failWith = 502 to "upstream_unavailable"
        val message = nextMessage()
        sonos.start(couch, episode, 90_000)
        assertEquals("Couldn't reach Couch.", message.await())
        waitFor("local again") { sonos.state.value.output == Output.Local }
    }

    @Test
    fun theTransportSendsCommandsAndSkipsByTheSyncedInterval() {
        handedOff()
        sonos.toggle()
        waitFor("the pause") { commandsSent().size == 1 }
        waitFor("shown paused") { sonos.state.value.session?.playing == false }
        sonos.skip(-15_000, durationMs = 3_600_000)
        sonos.seek(600_000)
        waitFor("three commands") { commandsSent().size == 3 }
        assertEquals(listOf(SonosCommand.Pause, SonosCommand.Skip(-15_000), SonosCommand.Seek(600_000)), commandsSent())
    }

    @Test
    fun aCommandTheSpeakerRefusesShowsTheSessionAsItWas() = runBlocking {
        handedOff()
        server.failWith = 502 to "upstream_unavailable"
        val message = nextMessage()
        sonos.toggle()
        assertEquals("Couldn't reach Couch.", message.await())
        waitFor("playing again") { sonos.state.value.session?.playing == true }
        assertTrue(sonos.state.value.output is Output.Sonos)
    }

    @Test
    fun noSessionLeavesRemoteModeAndAsksForASync() {
        handedOff()
        server.failWith = 409 to "no_session"
        sonos.toggle()
        waitFor("local again") { sonos.state.value.output == Output.Local }
        assertEquals(1, syncs.get())
        assertNull(sonos.state.value.session)
    }

    @Test
    fun aPollThatSeesTheGroupPlaySomethingElseLeavesRemoteMode() {
        handedOff()
        server.players = listOf(SonosPlayer(couch.id, couch.name, 12, SonosPlayerState.Other))
        waitFor("the 2 s poll", timeoutMs = 4_000) { sonos.state.value.output == Output.Local }
        assertEquals(1, syncs.get())
    }

    @Test
    fun aPollShowsTheSessionAsTheServerSeesIt() {
        handedOff()
        server.players = listOf(playing(positionMs = 120_000, playing = false, volume = 40))
        waitFor("the 2 s poll", timeoutMs = 4_000) { sonos.state.value.session?.playing == false }
        assertEquals(120_000, sonos.state.value.session!!.positionMs)
        assertEquals(40, sonos.state.value.volume)
    }

    @Test
    fun releasingHandsBackWhereTheSessionStood() = runBlocking {
        handedOff()
        server.released = SonosReleased(episode, 700_000, playing = true)
        assertEquals(Released(episode, 700_000, playing = true), sonos.release())
        assertEquals(Output.Local, sonos.state.value.output)
        assertEquals(listOf("POST /v1/sonos/players/${couch.id}/release"), server.log)
    }

    @Test
    fun releasingASessionThatEndedElsewhereGoesOnPausedWhereItWasShown() = runBlocking {
        handedOff()
        server.failWith = 409 to "no_session"
        val released = sonos.release()!!
        assertEquals(false, released.playing)
        assertTrue("${released.positionMs}", released.positionMs >= 90_000)
        assertEquals(Output.Local, sonos.state.value.output)
        assertEquals(1, syncs.get())
    }

    @Test
    fun releasingFromASpeakerThatDoesntAnswerKeepsRemoteMode() = runBlocking {
        handedOff()
        server.failWith = 502 to "upstream_unavailable"
        assertNull(sonos.release())
        assertTrue(sonos.state.value.output is Output.Sonos)
    }

    @Test
    fun aDraggedVolumeSendsOnlyTheNewestValueWhileOneIsOut() {
        handedOff()
        val gate = CountDownLatch(1)
        server.volumeGate = gate
        sonos.setVolume(10)
        check(server.volumeArrived.await(5, TimeUnit.SECONDS)) { "the first volume command never arrived" }
        sonos.setVolume(20)
        sonos.setVolume(30)
        waitFor("the shown volume") { sonos.state.value.volume == 30 }
        gate.countDown()
        waitFor("the second command") { commandsSent().size == 2 }
        Thread.sleep(200)
        assertEquals(listOf(SonosCommand.Volume(10), SonosCommand.Volume(30)), commandsSent())
    }

    @Test
    fun aPolledVolumeDoesntUndoTheUsersChangeForThreeSeconds() {
        handedOff()
        sonos.setVolume(50)
        waitFor("the volume command") { commandsSent().isNotEmpty() }
        // The server still reports the old volume, as a poll sent before the change would.
        server.log.clear()
        waitFor("a poll", timeoutMs = 4_000) { server.log.any { it.startsWith("GET") } }
        Thread.sleep(50)
        assertEquals(50, sonos.state.value.volume)
    }
}
