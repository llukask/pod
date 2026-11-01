package ltd.kilian.pod.connect

import android.util.Log
import java.io.IOException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerializationException
import ltd.kilian.pod.core.CONNECT_SILENCE_LIMIT_MS
import ltd.kilian.pod.core.ConnectCommand
import ltd.kilian.pod.core.ConnectEvent
import ltd.kilian.pod.core.ConnectReport
import ltd.kilian.pod.core.ConnectState
import ltd.kilian.pod.core.DeviceKind
import ltd.kilian.pod.core.ErrorBody
import ltd.kilian.pod.core.Playback
import ltd.kilian.pod.core.SonosPlayer
import ltd.kilian.pod.core.SonosPlayersResponse
import ltd.kilian.pod.core.SseParser
import ltd.kilian.pod.core.TransferRequest
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.core.decodeConnectEvent
import ltd.kilian.pod.net.JSON
import ltd.kilian.pod.net.fetch
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.sync.monotonicMs
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

/**
 * A Connect request that got no usable answer. [status] is null when pod-server itself didn't
 * answer; [code] is the error body's code, an open set (spec 4).
 */
class ConnectException(val status: Int?, val code: String?, message: String) : Exception(message) {
    /** `409 no_session`: nothing is active (any more), or this device has no open stream. */
    val noSession: Boolean get() = code == "no_session"

    /** What the snackbar says when a request for the device or group named [name] fails. */
    fun describe(name: String): String = when {
        status == 502 || status == null -> "Couldn't reach $name."
        else -> message ?: "Couldn't play on $name."
    }
}

/** How an event stream ended. */
enum class StreamEnd {
    /** It closed or failed; reconnect after the delay. */
    Closed,

    /** The server answered `401`: the device is signed out (spec 5.7). */
    Unauthorized,
}

/** What a command's answer says (14.4). */
data class CommandAnswer(
    val playback: Playback,
    /**
     * `200`: a Sonos group ran it and [playback] shows the result. `202`: it went to a client
     * device, which shows the change in its next report, so [playback] is still from before it.
     */
    val applied: Boolean,
)

/** The Connect endpoints of spec 14 and the Sonos player list of 13.2. Each call throws [ConnectException] on failure. */
interface ConnectServer {
    /**
     * Holds `GET /v1/connect/events` open until it ends or the caller is cancelled. [onOpen] runs
     * once the server accepted it, then [onEvent] for each event this client knows, in order.
     */
    suspend fun events(onOpen: suspend () -> Unit, onEvent: suspend (ConnectEvent) -> Unit): StreamEnd

    suspend fun report(report: ConnectReport): Playback

    suspend fun command(command: ConnectCommand): CommandAnswer

    suspend fun transfer(deviceId: String): Playback

    suspend fun state(): ConnectState

    /** Every Sonos group of the household. Empty when the server has Sonos off. */
    suspend fun players(): List<SonosPlayer>
}

/** [ConnectServer] on the signed-in account's server. */
class HttpConnectServer(
    private val database: PodDatabase,
    private val http: OkHttpClient,
    private val clock: () -> Long = ::monotonicMs,
) : ConnectServer {
    /**
     * The stream may stay quiet between pings, and lives as long as it is held, so it has neither
     * a read nor a call timeout. The silence watchdog in [events] stands in for them.
     */
    private val streaming = http.newBuilder().readTimeout(0, TimeUnit.MILLISECONDS).callTimeout(0, TimeUnit.MILLISECONDS).build()

    private data class Account(val serverUrl: String, val token: String)

    private suspend fun account(): Account? = withContext(Dispatchers.IO) {
        database.openHelper.readableDatabase.queryOne("select server_url, token from account where id = 1") {
            Account(it.string("server_url").trimEnd('/'), it.string("token"))
        }
    }

    override suspend fun events(onOpen: suspend () -> Unit, onEvent: suspend (ConnectEvent) -> Unit): StreamEnd = coroutineScope {
        val account = account() ?: return@coroutineScope StreamEnd.Unauthorized
        val request = Request.Builder()
            .url("${account.serverUrl}/v1/connect/events?kind=${DeviceKind.Android.wire}")
            .header("Authorization", "Bearer ${account.token}")
            .header("Accept", "text/event-stream")
            .build()
        val call = streaming.newCall(request)
        val heard = AtomicLong(clock())
        // The read blocks a thread, which a cancelled coroutine can't interrupt; cancelling the call can.
        val canceller = launch {
            try {
                awaitCancellation()
            } finally {
                call.cancel()
            }
        }
        val watchdog = launch {
            while (true) {
                delay(CONNECT_SILENCE_LIMIT_MS / 3)
                if (clock() - heard.get() > CONNECT_SILENCE_LIMIT_MS) {
                    Log.i(TAG, "the event stream went silent; reconnecting")
                    call.cancel()
                    return@launch
                }
            }
        }
        try {
            withContext(Dispatchers.IO) {
                call.execute().use { response ->
                    if (response.code == 401) return@withContext StreamEnd.Unauthorized
                    if (!response.isSuccessful) {
                        Log.i(TAG, "the server refused the event stream: ${response.code}")
                        return@withContext StreamEnd.Closed
                    }
                    onOpen()
                    val source = response.body!!.source()
                    val parser = SseParser()
                    val chunk = ByteArray(8_192)
                    while (true) {
                        val read = source.read(chunk)
                        if (read == -1) break
                        heard.set(clock())
                        for (data in parser.feed(chunk, read)) {
                            val event = decodeConnectEvent(data)
                            if (event == null) Log.i(TAG, "skipping an event this client doesn't know: ${data.take(80)}") else onEvent(event)
                        }
                    }
                    StreamEnd.Closed
                }
            }
        } catch (e: IOException) {
            Log.i(TAG, "the event stream closed: $e")
            StreamEnd.Closed
        } finally {
            canceller.cancel()
            watchdog.cancel()
        }
    }

    override suspend fun report(report: ConnectReport): Playback =
        post("report", WireJson.encodeToString(ConnectReport.serializer(), report), Playback.serializer()).first

    override suspend fun command(command: ConnectCommand): CommandAnswer {
        val (playback, status) = post("command", WireJson.encodeToString(ConnectCommand.serializer(), command), Playback.serializer())
        return CommandAnswer(playback, applied = status == 200)
    }

    override suspend fun transfer(deviceId: String): Playback =
        post("transfer", WireJson.encodeToString(TransferRequest.serializer(), TransferRequest(deviceId)), Playback.serializer()).first

    override suspend fun state(): ConnectState = request("GET", "/v1/connect", null, ConnectState.serializer()).first

    override suspend fun players(): List<SonosPlayer> = request("GET", "/v1/sonos/players", null, SonosPlayersResponse.serializer()).first.players

    private suspend fun <T> post(endpoint: String, body: String, serializer: KSerializer<T>): Pair<T, Int> =
        request("POST", "/v1/connect/$endpoint", body, serializer)

    private suspend fun <T> request(method: String, path: String, body: String?, serializer: KSerializer<T>): Pair<T, Int> {
        val account = account() ?: throw ConnectException(401, "unauthenticated", "Sign in again to play on another device.")
        val request = Request.Builder().url(account.serverUrl + path).header("Authorization", "Bearer ${account.token}")
            .method(method, body?.toRequestBody(JSON))
            .build()
        val fetched = try {
            http.fetch(request)
        } catch (e: IOException) {
            Log.i(TAG, "the Connect request failed: $e")
            throw ConnectException(null, null, "pod-server didn't answer")
        }
        if (fetched.isSuccessful) {
            val decoded = try {
                WireJson.decodeFromString(serializer, fetched.body)
            } catch (e: SerializationException) {
                Log.w(TAG, "the Connect response doesn't parse: $e")
                null
            } catch (e: IllegalArgumentException) {
                Log.w(TAG, "the Connect response doesn't parse: $e")
                null
            }
            return (decoded ?: throw ConnectException(fetched.status, null, "The server's answer doesn't parse.")) to fetched.status
        }
        val error = runCatching { WireJson.decodeFromString(ErrorBody.serializer(), fetched.body).error }.getOrNull()
        Log.i(TAG, "the server failed the Connect request: ${fetched.status} ${error?.code}")
        throw ConnectException(fetched.status, error?.code, error?.message ?: "The server answered ${fetched.status}.")
    }
}

private const val TAG = "PodConnect"
