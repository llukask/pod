package ltd.kilian.pod.episode

import android.util.Log
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
import ltd.kilian.pod.core.Chapter
import ltd.kilian.pod.core.ChapterList
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.fetch
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import okhttp3.OkHttpClient
import okhttp3.Request

// ==============================================================================================
// Chapters (spec 17.4)
// ==============================================================================================
//
// The server resolves an episode's chapters on demand, so the episode screen asks for them when
// it opens. No table stores them: they live in memory for the process, and an offline device or
// a failed request shows none.

private const val TAG = "PodChapters"

/** The chapters each episode had when asked, for the life of the process. */
class Chapters(private val database: PodDatabase, private val http: OkHttpClient) {
    private val known = ConcurrentHashMap<String, List<Chapter>>()

    /** What an earlier [load] found for [episodeId], without a request. */
    fun cached(episodeId: String): List<Chapter>? = known[episodeId]

    /**
     * `GET /v1/episodes/{id}/chapters`, or the remembered answer. Null when the request fails or
     * the device is signed out; a failure isn't remembered, so the next visit asks again.
     */
    suspend fun load(episodeId: String): List<Chapter>? {
        known[episodeId]?.let { return it }
        val account = withContext(Dispatchers.IO) {
            database.openHelper.readableDatabase.queryOne("select server_url, token from account where id = 1") {
                it.string("server_url") to it.string("token")
            }
        } ?: return null
        val (serverUrl, token) = account
        val request = Request.Builder()
            .url(serverUrl.trimEnd('/') + "/v1/episodes/$episodeId/chapters")
            .header("Authorization", "Bearer $token")
            .build()
        val fetched = try {
            http.fetch(request)
        } catch (e: IOException) {
            Log.i(TAG, "the chapters request failed: $e")
            return null
        }
        if (!fetched.isSuccessful) {
            Log.i(TAG, "the server failed the chapters request: ${fetched.status}")
            return null
        }
        val chapters = try {
            WireJson.decodeFromString(ChapterList.serializer(), fetched.body).chapters
        } catch (e: SerializationException) {
            Log.w(TAG, "the chapters response doesn't parse: $e")
            return null
        } catch (e: IllegalArgumentException) {
            Log.w(TAG, "the chapters response doesn't parse: $e")
            return null
        }
        known[episodeId] = chapters
        return chapters
    }
}

/** A chapter's start as the list shows it: `M:SS`, or `H:MM:SS` from an hour on. */
fun chapterStartLabel(startMs: Long): String {
    val total = maxOf(0, startMs) / 1_000
    val hours = total / 3_600
    val minutes = total % 3_600 / 60
    val seconds = total % 60
    fun two(value: Long) = value.toString().padStart(2, '0')
    return if (hours > 0) "$hours:${two(minutes)}:${two(seconds)}" else "$minutes:${two(seconds)}"
}

/**
 * The index of the chapter that holds [positionMs], the last one starting at or before it, or
 * null before the first chapter starts. [chapters] are sorted by start, as the server sends them.
 */
fun currentChapter(chapters: List<Chapter>, positionMs: Long): Int? =
    chapters.indexOfLast { it.startMs <= positionMs }.takeIf { it >= 0 }
