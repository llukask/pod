package ltd.kilian.pod.net

import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Headers
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response

val JSON = "application/json".toMediaType()

/**
 * The client for requests to the account's server. Bounds a request that hangs, so the sync
 * loop backs off instead of holding its mutex forever.
 */
fun podHttpClient(): OkHttpClient = OkHttpClient.Builder()
    .connectTimeout(15, TimeUnit.SECONDS)
    .callTimeout(60, TimeUnit.SECONDS)
    .build()

/** A whole response, read off the calling thread. */
class Fetched(val status: Int, val headers: Headers, val body: String) {
    val isSuccessful get() = status in 200..299
}

/**
 * Sends [request] and reads the whole body. Cancelling the coroutine cancels the call, which is
 * how logout drops the sync request in flight (spec 5.7). Throws [IOException] when no response
 * arrives.
 */
suspend fun OkHttpClient.fetch(request: Request): Fetched = suspendCancellableCoroutine { continuation ->
    val call = newCall(request)
    continuation.invokeOnCancellation { call.cancel() }
    call.enqueue(object : Callback {
        override fun onFailure(call: Call, e: IOException) = continuation.resumeWithException(e)

        override fun onResponse(call: Call, response: Response) {
            val fetched = try {
                response.use { Fetched(it.code, it.headers, it.body?.string().orEmpty()) }
            } catch (e: IOException) {
                continuation.resumeWithException(e)
                return
            }
            continuation.resume(fetched)
        }
    })
}
