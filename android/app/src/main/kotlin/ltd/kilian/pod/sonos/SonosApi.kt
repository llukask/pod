package ltd.kilian.pod.sonos

import android.util.Log
import java.io.IOException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerializationException
import ltd.kilian.pod.core.ErrorBody
import ltd.kilian.pod.core.SonosCommand
import ltd.kilian.pod.core.SonosPlayRequest
import ltd.kilian.pod.core.SonosPlayer
import ltd.kilian.pod.core.SonosPlayersResponse
import ltd.kilian.pod.core.SonosReleased
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.JSON
import ltd.kilian.pod.net.fetch
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

/**
 * A Sonos request that got no usable answer. [status] is null when pod-server itself didn't
 * answer; [code] is the error body's code, an open set (spec 4).
 */
class SonosException(val status: Int?, val code: String?, message: String) : Exception(message) {
    /** `409 no_session`, or `404` for a group that is no longer a coordinator: the session is over. */
    val sessionGone: Boolean get() = code == "no_session" || status == 404

    /** What the snackbar says when a request to the group named [name] fails. */
    fun describe(name: String): String = when {
        status == 502 -> "Couldn't reach $name."
        status == null -> "Couldn't play on $name."
        else -> message ?: "Couldn't play on $name."
    }
}

/** The Sonos endpoints of spec 13.2 on the account's server. Each call throws [SonosException] on failure. */
class SonosApi(private val database: PodDatabase, private val http: OkHttpClient) {
    /** Every group of the household. Empty when the server has Sonos off. */
    suspend fun players(): List<SonosPlayer> = request("GET", emptyList(), null, SonosPlayersResponse.serializer()).players

    suspend fun play(playerId: String, episodeId: String, positionMs: Long): SonosPlayer =
        request("POST", listOf(playerId, "play"), WireJson.encodeToString(SonosPlayRequest.serializer(), SonosPlayRequest(episodeId, positionMs)), SonosPlayer.serializer())

    suspend fun command(playerId: String, command: SonosCommand): SonosPlayer =
        request("POST", listOf(playerId, "command"), WireJson.encodeToString(SonosCommand.serializer(), command), SonosPlayer.serializer())

    suspend fun release(playerId: String): SonosReleased = request("POST", listOf(playerId, "release"), "", SonosReleased.serializer())

    private suspend fun <T> request(method: String, segments: List<String>, body: String?, serializer: KSerializer<T>): T {
        val (serverUrl, token) = withContext(Dispatchers.IO) {
            database.openHelper.readableDatabase.queryOne("select server_url, token from account where id = 1") { it.string("server_url") to it.string("token") }
        } ?: throw SonosException(401, "unauthenticated", "Sign in again to play on a speaker.")
        // The player id goes in as one encoded path segment, so no id can reach another endpoint.
        val url = (serverUrl.trimEnd('/') + "/v1/sonos/players").toHttpUrl().newBuilder()
            .apply { segments.forEach(::addPathSegment) }
            .build()
        val request = Request.Builder().url(url).header("Authorization", "Bearer $token")
            .method(method, body?.toRequestBody(JSON))
            .build()
        val fetched = try {
            http.fetch(request)
        } catch (e: IOException) {
            Log.i(TAG, "the Sonos request failed: $e")
            throw SonosException(null, null, "pod-server didn't answer")
        }
        if (fetched.isSuccessful) {
            val decoded = try {
                WireJson.decodeFromString(serializer, fetched.body)
            } catch (e: SerializationException) {
                Log.w(TAG, "the Sonos response doesn't parse: $e")
                null
            } catch (e: IllegalArgumentException) {
                Log.w(TAG, "the Sonos response doesn't parse: $e")
                null
            }
            return decoded ?: throw SonosException(fetched.status, null, "The server's answer doesn't parse.")
        }
        val error = runCatching { WireJson.decodeFromString(ErrorBody.serializer(), fetched.body).error }.getOrNull()
        Log.i(TAG, "the server failed the Sonos request: ${fetched.status} ${error?.code}")
        throw SonosException(fetched.status, error?.code, error?.message ?: "The server answered ${fetched.status}.")
    }
}

private const val TAG = "PodSonos"
