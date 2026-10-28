package ltd.kilian.pod.search

import android.util.Log
import androidx.sqlite.db.SupportSQLiteDatabase
import java.io.IOException
import java.net.ConnectException
import java.net.UnknownHostException
import java.util.concurrent.TimeUnit
import kotlinx.serialization.SerializationException
import ltd.kilian.pod.core.DirectoryResult
import ltd.kilian.pod.core.DirectorySearchResponse
import ltd.kilian.pod.core.ErrorBody
import ltd.kilian.pod.core.FeedUrl
import ltd.kilian.pod.core.FeedUrlError
import ltd.kilian.pod.core.FeedUrlResult
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.fetch
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request

// ==============================================================================================
// Search and add (wireframe M4, spec 4.6 and 5.5)
// ==============================================================================================
//
// What the search field holds, whether a result is already followed, and the directory request.
// Search needs the network; subscribing doesn't. A port of pod-desktop's `search/mod.rs`.

/** Bounds a directory search, which waits on a third-party directory behind the server. */
private const val SEARCH_TIMEOUT_S = 20L

/** The search field's text, read as either a feed to subscribe to or terms to search the directory for. */
sealed interface Query {
    data object Empty : Query

    /** A URL, normalized for the offline subscribe path, or why it can't be. */
    data class Feed(val result: FeedUrlResult) : Query

    /** Trimmed search terms. */
    data class Terms(val terms: String) : Query
}

/**
 * Decides whether [input] is a feed URL or search terms.
 *
 * It is a URL when it names a scheme (`://`), so `feed://…` reaches the normalizer and gets its
 * inline error instead of a search. Without a scheme it is a URL only when it reads as a host
 * and a path: no whitespace, and a dot before the first `/`, as in `feeds.example.com/show.xml`;
 * that form gets `https://`. A bare domain such as `syntax.fm` stays a search, since a site's
 * home page is rarely its feed and the name finds the show.
 */
fun classify(input: String): Query {
    val text = input.trim()
    if (text.isEmpty()) return Query.Empty
    if ("://" in text) return Query.Feed(FeedUrl.normalize(text))
    val hostAndPath = text.substringBefore('/', missingDelimiterValue = "").contains('.') && text.none { it.isWhitespace() }
    if (hostAndPath) return Query.Feed(FeedUrl.normalize("https://$text"))
    return Query.Terms(text)
}

/**
 * Whether the user already follows the show behind [result], by a local lookup in [subscribed].
 * The result's `podcast_id` is the server's canonical id, so a directory URL that is an old alias
 * of a followed show matches it (spec 4.6). The id derived from the result's own URL matches
 * too: that is the placeholder subscribing from this result creates (spec 5.5).
 */
fun isSubscribed(result: DirectoryResult, subscribed: Set<String>): Boolean =
    result.podcastId in subscribed ||
        (FeedUrl.normalize(result.feedUrl) as? FeedUrlResult.Ok)?.url?.podcastId in subscribed

/** What a feed URL that doesn't normalize says under the field. The URL stays out of it: it may carry credentials. */
fun feedUrlProblem(error: FeedUrlError): String = when (error) {
    FeedUrlError.UnsupportedScheme -> "Feed addresses start with http:// or https://."
    else -> "That isn't a web address."
}

/** The shows the user follows, for [isSubscribed]. */
fun loadSubscribed(db: SupportSQLiteDatabase): Set<String> =
    db.queryList("select podcast_id from subscription where subscribed = 1") { it.string("podcast_id") }.toSet()

/** Why a search found nothing to show, worded for the results. */
sealed class SearchError(val message: String) {
    /** No network, or the server can't be connected to. */
    data object Offline : SearchError("Search needs a connection. You can still add a show by its feed URL. It syncs when you're back online.")

    /** `upstream_unavailable`: the server is fine, the directory isn't. */
    data object Upstream : SearchError("The podcast directory isn't answering right now. Try again in a moment.")
    data object SignedOut : SearchError("Sign in again to search.")
    data object RateLimited : SearchError("Too many searches at once. Try again in a minute.")

    /** The request went out but no usable answer came back. */
    data object Unreachable : SearchError("The server didn't answer the search.")

    /** Another status. The error code set is open, so the status decides. */
    data class Server(val status: Int) : SearchError("The search failed: the server answered $status.")
}

sealed interface SearchResult {
    data class Ok(val results: List<DirectoryResult>) : SearchResult

    data class Failed(val error: SearchError) : SearchResult
}

/**
 * `GET /v1/directory/search` on the account's server (spec 4.6). [connected] is the OS network
 * state; without one the search fails at once without a request.
 */
suspend fun search(db: SupportSQLiteDatabase, http: OkHttpClient, connected: Boolean, terms: String): SearchResult {
    if (!connected) return SearchResult.Failed(SearchError.Offline)
    val (serverUrl, token) = db.queryOne("select server_url, token from account where id = 1") { it.string("server_url") to it.string("token") }
        ?: return SearchResult.Failed(SearchError.SignedOut)
    val url = (serverUrl.trimEnd('/') + "/v1/directory/search").toHttpUrl().newBuilder().addQueryParameter("q", terms).build()
    val request = Request.Builder().url(url).header("Authorization", "Bearer $token").build()
    val client = http.newBuilder().callTimeout(SEARCH_TIMEOUT_S, TimeUnit.SECONDS).build()
    val fetched = try {
        client.fetch(request)
    } catch (e: IOException) {
        Log.i(TAG, "the search request failed: $e")
        return SearchResult.Failed(if (e is ConnectException || e is UnknownHostException) SearchError.Offline else SearchError.Unreachable)
    }
    if (fetched.isSuccessful) {
        return try {
            SearchResult.Ok(WireJson.decodeFromString(DirectorySearchResponse.serializer(), fetched.body).results)
        } catch (e: SerializationException) {
            Log.w(TAG, "the search response doesn't parse: $e")
            SearchResult.Failed(SearchError.Unreachable)
        } catch (e: IllegalArgumentException) {
            Log.w(TAG, "the search response doesn't parse: $e")
            SearchResult.Failed(SearchError.Unreachable)
        }
    }
    val code = runCatching { WireJson.decodeFromString(ErrorBody.serializer(), fetched.body).error.code }.getOrNull()
    Log.i(TAG, "the server failed the search: ${fetched.status} $code")
    return SearchResult.Failed(
        when {
            code == "upstream_unavailable" -> SearchError.Upstream
            fetched.status == 401 -> SearchError.SignedOut
            fetched.status == 429 -> SearchError.RateLimited
            else -> SearchError.Server(fetched.status)
        },
    )
}

private const val TAG = "PodSearch"
