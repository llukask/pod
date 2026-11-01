package ltd.kilian.pod.connect

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import java.util.Collections
import kotlinx.coroutines.runBlocking
import ltd.kilian.pod.core.ConnectCommand
import ltd.kilian.pod.core.ConnectEvent
import ltd.kilian.pod.core.ConnectReport
import ltd.kilian.pod.core.ErrorBody
import ltd.kilian.pod.core.ErrorDetail
import ltd.kilian.pod.core.Playback
import ltd.kilian.pod.core.PlaybackEpisode
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.JSON
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/** The Connect endpoints over HTTP, against an OkHttp interceptor standing in for pod-server. */
@RunWith(RobolectricTestRunner::class)
class HttpConnectServerTest {
    private val episode = "0192f0a0-0000-7000-8000-000000000001"
    private val playback = Playback(null, PlaybackEpisode(episode, "0192f0a0-0000-7000-8000-000000000002", "Episode"), 42_000, false, 1.0)

    /** How each path answers: a status and a body. */
    private val answers = mutableMapOf<String, Pair<Int, String>>()

    /** Every request as "METHOD path?query body". */
    private val log: MutableList<String> = Collections.synchronizedList(mutableListOf())

    private val interceptor = Interceptor { chain ->
        val request = chain.request()
        val body = request.body?.let { b -> Buffer().also { b.writeTo(it) }.readUtf8() }.orEmpty()
        log += "${request.method} ${request.url.encodedPath}${request.url.encodedQuery?.let { "?$it" }.orEmpty()} $body".trim()
        val (code, json) = answers[request.url.encodedPath] ?: (404 to "")
        val type = if (request.url.encodedPath.endsWith("/events") && code == 200) "text/event-stream".toMediaType() else JSON
        Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(code).message("").body(json.toResponseBody(type)).build()
    }

    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java).build()
    private val server = HttpConnectServer(database, OkHttpClient.Builder().addInterceptor(interceptor).build())

    @Before
    fun signIn() {
        database.openHelper.writableDatabase.exec(
            """insert into account (id, server_url, user_id, username, device_id, token)
               values (1, 'https://pod.example/', '0192f0a1-0000-7000-8000-000000000001', 'alice',
                       '0192f0a1-0000-7000-8000-000000000002', 'token')""",
        )
    }

    @After
    fun close() {
        database.close()
    }

    @Test
    fun theStreamDeliversEachKnownEventInOrderUntilItCloses() = runBlocking {
        answers["/v1/connect/events"] = 200 to """
            |: ping
            |
            |data: {"type": "state", "devices": [], "playback": null}
            |
            |data: {"type": "shuffle"}
            |
            |data: {"type": "release", "transfer_id": "t-1"}
            |
            |""".trimMargin()
        val seen = mutableListOf<String>()
        val end = server.events(onOpen = { seen += "open" }, onEvent = { seen += it.toString() })
        assertEquals(StreamEnd.Closed, end)
        assertEquals(listOf("open", ConnectEvent.State(emptyList(), null).toString(), ConnectEvent.Release("t-1").toString()), seen)
        assertEquals(listOf("GET /v1/connect/events?kind=android"), log)
    }

    @Test
    fun aStreamTheServerRefusesEndsSignedOutOrClosed() = runBlocking {
        answers["/v1/connect/events"] = 401 to """{"error": {"code": "unauthenticated", "message": "no"}}"""
        var opened = false
        assertEquals(StreamEnd.Unauthorized, server.events(onOpen = { opened = true }, onEvent = {}))
        answers["/v1/connect/events"] = 503 to ""
        assertEquals(StreamEnd.Closed, server.events(onOpen = { opened = true }, onEvent = {}))
        assertFalse(opened)
    }

    @Test
    fun aCommandSaysWhetherItRanOrWasPassedOn() = runBlocking {
        val body = WireJson.encodeToString(Playback.serializer(), playback)
        answers["/v1/connect/command"] = 202 to body
        assertEquals(CommandAnswer(playback, applied = false), server.command(ConnectCommand.Pause))
        answers["/v1/connect/command"] = 200 to body
        assertEquals(CommandAnswer(playback, applied = true), server.command(ConnectCommand.Volume(30)))
        assertEquals("""POST /v1/connect/command {"type":"pause"}""", log.first())
    }

    @Test
    fun aRefusedRequestCarriesItsCode() = runBlocking {
        answers["/v1/connect/report"] = 409 to WireJson.encodeToString(ErrorBody.serializer(), ErrorBody(ErrorDetail("no_session", "no stream")))
        val refused = runCatching { server.report(ConnectReport(episode, 0, true, 1.0)) }.exceptionOrNull() as ConnectException
        assertTrue(refused.noSession)
        assertEquals(409, refused.status)
        answers["/v1/connect/transfer"] = 200 to WireJson.encodeToString(Playback.serializer(), playback)
        assertEquals(playback, server.transfer("RINCON_1"))
        assertEquals("""POST /v1/connect/transfer {"device_id":"RINCON_1"}""", log.last())
    }
}
