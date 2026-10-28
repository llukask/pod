package ltd.kilian.pod.sync

import android.util.Log
import androidx.sqlite.db.SupportSQLiteDatabase
import java.io.IOException
import java.security.cert.CertPathValidatorException
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLException
import javax.net.ssl.SSLPeerUnverifiedException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
import ltd.kilian.pod.core.AuthRequest
import ltd.kilian.pod.core.ErrorBody
import ltd.kilian.pod.core.TokenResponse
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.JSON
import ltd.kilian.pod.net.fetch
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

// ==============================================================================================
// Signing in and out (spec 4.1, 5.7)
// ==============================================================================================
//
// The `account` row is the session: server URL, user, device id and bearer token (spec 5.1).
// Its presence decides whether the app shows sign-in or the inbox, and its `user_id` is what the
// sync apply step checks a response against.
//
// A `401` doesn't come through here. The engine keeps the row and reports
// [EngineState.SignedOut]; signing in again as the same user only swaps the token and device
// id, so the outbox survives and sync resumes. Signing in as anyone else logs out first. A port
// of pod-desktop's `sync/session.rs`.

/** Shown in the user's session list on the server. */
private val DEVICE_NAME = "Pod on ${android.os.Build.MODEL}"

/** Bounds the best-effort `POST /v1/auth/logout`. */
private const val LOGOUT_REQUEST_TIMEOUT_S = 10L

/** The signed-in account, as the UI shows it. */
data class Account(val serverUrl: String, val username: String)

/** What the user typed into the sign-in form. */
data class Credentials(val serverUrl: String, val username: String, val password: String, val register: Boolean) {
    override fun toString() = "Credentials(serverUrl=$serverUrl, username=$username, password=<redacted>, register=$register)"
}

enum class SignedIn {
    /** There was no account. */
    New,

    /** The same user signed in again, after a `401`. The outbox is kept. */
    Resumed,

    /** Another user's data was there, and logout cleared it first. */
    Switched,
}

/** Why signing in failed, worded for the form. */
sealed class SignInError(val message: String) {
    data object InvalidServerUrl : SignInError("That isn't a server address.")
    data object InvalidCredentials : SignInError("Username or password is wrong.")
    data object UsernameTaken : SignInError("That username is taken.")
    data object RegistrationDisabled : SignInError("This server doesn't accept new accounts.")

    /** A register rule failed (spec 4.1). Holds the server's explanation. */
    data class Rejected(val explanation: String) : SignInError(explanation)
    data object RateLimited : SignInError("Too many attempts. Try again in a minute.")
    data object Unreachable : SignInError("Couldn't reach the server.")

    /** The TLS handshake failed, as it does against a server that only speaks plain HTTP. */
    data object NoHttps : SignInError("The server didn't answer over HTTPS. If it only serves HTTP, include http:// in the address.")
    data object UntrustedCertificate : SignInError("The server's HTTPS certificate isn't trusted.")

    /** Another status, or a body that isn't a token response. */
    data class Server(val status: Int) : SignInError("The server answered $status.")
}

sealed interface SignInResult {
    data class Ok(val signedIn: SignedIn) : SignInResult

    data class Failed(val error: SignInError) : SignInResult
}

/**
 * Turns what the user typed into a base URL: `https://` when no scheme is given, no trailing
 * slash. Null when it isn't an http(s) URL.
 */
fun normalizeServerUrl(input: String): String? {
    val trimmed = input.trim().trimEnd('/')
    val withScheme = if ("://" in trimmed) trimmed else "https://$trimmed"
    // `toHttpUrlOrNull` only accepts http and https, and lowercases the scheme.
    val url = withScheme.toHttpUrlOrNull() ?: return null
    if (url.host.isEmpty() || url.query != null || url.fragment != null) return null
    return withScheme
}

fun loadAccount(db: SupportSQLiteDatabase): Account? =
    db.queryOne("select server_url, username from account where id = 1") { Account(it.string("server_url"), it.string("username")) }

/** What logout throws away, for its confirmation (wireframe M11). */
data class LogoutSummary(val unsent: Long, val downloadBytes: Long)

fun loadLogoutSummary(db: SupportSQLiteDatabase): LogoutSummary =
    db.queryOne(
        "select (select count(*) from outbox) as unsent, (select coalesce(sum(bytes_done), 0) from download) as bytes",
    ) { LogoutSummary(it.long("unsent"), it.long("bytes")) }!!

/**
 * Logout's table wipe (spec 5.7): every table except `device_setting`, which belongs to the
 * device. That includes `setting`, the user's synced settings, and `podcast_device`, which
 * belongs to that user's podcasts. Unsent outbox rows are lost; the logout dialog says so.
 */
fun deleteAccountData(db: SupportSQLiteDatabase) {
    PodDatabase.TABLES.filter { it != "device_setting" }.forEach { db.exec("delete from $it") }
    // The remembered episode is the account's.
    db.exec("delete from device_setting where key = ?", LAST_EPISODE_ID)
}

/** Signs in and out. One per process. */
class Session(
    private val database: PodDatabase,
    private val engine: SyncEngine,
    /**
     * Logout's step 2: cancels every download job and waits for them to stop, at most 10 s, and
     * deletes the downloads and artwork directories after the wipe. Returns once both are done.
     */
    private val stopDownloads: suspend (wipe: suspend () -> Unit) -> Unit = { it() },
) {
    /** Logs in or registers, stores the account row and asks for a sync. */
    suspend fun signIn(credentials: Credentials): SignInResult {
        val serverUrl = normalizeServerUrl(credentials.serverUrl) ?: return SignInResult.Failed(SignInError.InvalidServerUrl)
        val token = when (val authenticated = authenticate(serverUrl, credentials)) {
            is Authenticated.Ok -> authenticated.token
            is Authenticated.Failed -> return SignInResult.Failed(authenticated.error)
        }
        val username = credentials.username.trim().lowercase()
        val signedIn = when (storeAccount(serverUrl, username, token)) {
            Stored.Inserted -> SignedIn.New
            Stored.Updated -> SignedIn.Resumed
            Stored.OtherUser -> {
                logout()
                check(storeAccount(serverUrl, username, token) == Stored.Inserted) { "the account row survived logout" }
                SignedIn.Switched
            }
        }
        Log.i(TAG, "signed in: $signedIn")
        engine.handle.requestSync(0)
        return SignInResult.Ok(signedIn)
    }

    private sealed interface Authenticated {
        data class Ok(val token: TokenResponse) : Authenticated

        data class Failed(val error: SignInError) : Authenticated
    }

    private suspend fun authenticate(serverUrl: String, credentials: Credentials): Authenticated {
        val path = if (credentials.register) "register" else "login"
        val body = AuthRequest(credentials.username, credentials.password, DEVICE_NAME)
        val request = Request.Builder()
            .url("$serverUrl/v1/auth/$path")
            .post(WireJson.encodeToString(AuthRequest.serializer(), body).toRequestBody(JSON))
            .build()
        val fetched = try {
            engine.http.fetch(request)
        } catch (e: IOException) {
            Log.i(TAG, "the sign-in request failed: $e")
            return Authenticated.Failed(requestFailure(e))
        }
        if (fetched.isSuccessful) {
            return try {
                Authenticated.Ok(WireJson.decodeFromString(TokenResponse.serializer(), fetched.body))
            } catch (e: SerializationException) {
                Log.w(TAG, "the sign-in response doesn't parse: $e")
                Authenticated.Failed(SignInError.Server(fetched.status))
            } catch (e: IllegalArgumentException) {
                Log.w(TAG, "the sign-in response doesn't parse: $e")
                Authenticated.Failed(SignInError.Server(fetched.status))
            }
        }
        val error = runCatching { WireJson.decodeFromString(ErrorBody.serializer(), fetched.body).error }.getOrNull()
        Log.i(TAG, "sign-in refused: ${fetched.status} ${error?.code}")
        // The code set is open (spec 4), so an unknown code falls back to its status.
        return Authenticated.Failed(
            when {
                error?.code == "username_taken" -> SignInError.UsernameTaken
                error?.code == "registration_disabled" -> SignInError.RegistrationDisabled
                fetched.status == 400 && error != null -> SignInError.Rejected(error.message)
                fetched.status == 401 -> SignInError.InvalidCredentials
                fetched.status == 429 -> SignInError.RateLimited
                else -> SignInError.Server(fetched.status)
            },
        )
    }

    /**
     * Why a request got no answer. A failed TLS handshake gets its own error, since the address
     * defaults to `https://` and a plain HTTP server fails exactly there.
     */
    private fun requestFailure(error: IOException): SignInError {
        val causes = generateSequence<Throwable>(error) { it.cause }.toList()
        return when {
            causes.any { it is SSLPeerUnverifiedException || it is CertPathValidatorException } -> SignInError.UntrustedCertificate
            causes.any { it is SSLException } -> SignInError.NoHttps
            else -> SignInError.Unreachable
        }
    }

    private enum class Stored { Inserted, Updated, OtherUser }

    /** Writes the account row unless another user's row is there. */
    private suspend fun storeAccount(serverUrl: String, username: String, token: TokenResponse): Stored = withContext(Dispatchers.IO) {
        database.runInTransaction<Stored> {
            val db = database.openHelper.writableDatabase
            when (db.queryOne("select user_id from account where id = 1") { it.string("user_id") }) {
                null -> {
                    db.exec(
                        "insert into account (id, server_url, user_id, username, device_id, token) values (1, ?, ?, ?, ?, ?)",
                        serverUrl, token.userId, username, token.deviceId, token.token,
                    )
                    Stored.Inserted
                }
                // Mutations already queued keep the device id they were stamped with; only new
                // ones get the new id (spec 5.7).
                token.userId -> {
                    db.exec(
                        "update account set server_url = ?, username = ?, device_id = ?, token = ? where id = 1",
                        serverUrl, username, token.deviceId, token.token,
                    )
                    Stored.Updated
                }
                else -> Stored.OtherUser
            }
        }
    }

    /**
     * Logs out (spec 5.7), in this order:
     *
     * 1. Take the sync mutex, cancelling the request in flight, so no response is applied after
     *    this.
     * 2. Cancel every download job and wait for them to stop ([stopDownloads]), so no transfer
     *    starts or requeues a row of the store being wiped.
     * 3. In one transaction, delete every table except `device_setting`.
     * 4. Delete the downloads directory and the artwork cache.
     * 5. `POST /v1/auth/logout`, best effort.
     *
     * Unsent outbox rows are lost; the confirmation said so.
     */
    suspend fun logout() {
        val account = engine.pause().use {
            var account: Pair<String, String>? = null
            stopDownloads {
                account = withContext(Dispatchers.IO) {
                    database.runInTransaction<Pair<String, String>?> {
                        val db = database.openHelper.writableDatabase
                        val row = db.queryOne("select server_url, token from account where id = 1") { it.string("server_url") to it.string("token") }
                        deleteAccountData(db)
                        row
                    }
                }
            }
            account
        }
        if (account != null) {
            val (serverUrl, token) = account
            val request = Request.Builder().url("$serverUrl/v1/auth/logout").header("Authorization", "Bearer $token")
                .post(ByteArray(0).toRequestBody(null)).build()
            val client = engine.http.newBuilder().callTimeout(LOGOUT_REQUEST_TIMEOUT_S, TimeUnit.SECONDS).build()
            try {
                val fetched = client.fetch(request)
                if (!fetched.isSuccessful) Log.i(TAG, "the server refused the logout: ${fetched.status}")
            } catch (e: IOException) {
                Log.i(TAG, "the logout request failed: $e")
            }
        }
        Log.i(TAG, "logged out")
    }

    private companion object {
        const val TAG = "PodSession"
    }
}

/** The `device_setting` key of the episode the player last held, restored at start. */
const val LAST_EPISODE_ID = "last_episode_id"
