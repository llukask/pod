package ltd.kilian.pod.sync

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import ltd.kilian.pod.core.TokenResponse
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.net.JSON
import ltd.kilian.pod.net.NetworkState
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class SessionTest {
    /** Answers every auth request with [status] and [body], and records the paths it saw. */
    private class Stub : Interceptor {
        @Volatile var status = 200
        @Volatile var body = ""
        val paths = mutableListOf<String>()

        override fun intercept(chain: Interceptor.Chain): Response {
            synchronized(paths) { paths += chain.request().url.encodedPath }
            return Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(status).message("")
                .body(body.toResponseBody(JSON)).build()
        }
    }

    private val stub = Stub()
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java).build()
    private val engine = SyncEngine(
        database, SyncHandle(), OkHttpClient.Builder().addInterceptor(stub).build(),
        // Offline, so a sign-in's sync request never reaches the stub.
        MutableStateFlow(NetworkState.OFFLINE),
    )
    private val session = Session(database, engine)
    private val db get() = database.openHelper.writableDatabase

    @After
    fun close() = database.close()

    private fun token(user: String, device: String) {
        stub.status = 200
        stub.body = WireJson.encodeToString(TokenResponse.serializer(), TokenResponse("token-$device", device, user, 1_900_000_000_000))
    }

    private fun refuse(status: Int, code: String, message: String = "nope") {
        stub.status = status
        stub.body = """{"error":{"code":"$code","message":"$message"}}"""
    }

    private val alice = Credentials("pod.example/", " Alice ", "hunter22", register = false)
    private fun count(sql: String) = db.queryOne(sql) { it.long("n") }!!

    @Test
    fun serverAddressesGetHttpsAndLoseTheTrailingSlash() {
        assertEquals("https://pod.example", normalizeServerUrl(" pod.example/ "))
        assertEquals("http://10.0.2.2:3000", normalizeServerUrl("http://10.0.2.2:3000"))
        assertEquals("https://pod.example/base", normalizeServerUrl("https://pod.example/base/"))
        assertNull(normalizeServerUrl("ftp://pod.example"))
        assertNull(normalizeServerUrl("https://pod.example/?q=1"))
        assertNull(normalizeServerUrl(""))
    }

    @Test
    fun signingInStoresTheAccountAndAsksForASync() = runBlocking {
        token("user-a", "device-1")
        assertEquals(SignInResult.Ok(SignedIn.New), session.signIn(alice))
        assertEquals(listOf("/v1/auth/login"), stub.paths)
        assertEquals(Account("https://pod.example", "alice"), loadAccount(db))
        assertEquals("device-1", db.queryOne("select device_id from account") { it.string("device_id") })
        assert(engine.handle.wanted.value != null)
    }

    @Test
    fun theSameUserResumesWithTheOutbox() = runBlocking {
        token("user-a", "device-1")
        session.signIn(alice)
        writeLocal(database, engine.handle, LocalChange.Setting("inbox_window_days", kotlinx.serialization.json.JsonPrimitive(7)))

        token("user-a", "device-2")
        assertEquals(SignInResult.Ok(SignedIn.Resumed), session.signIn(alice))
        assertEquals(1, count("select count(*) as n from outbox"))
        assertEquals("device-2", db.queryOne("select device_id from account") { it.string("device_id") })
    }

    @Test
    fun anotherUserLogsOutFirstAndKeepsOnlyDeviceSettings() = runBlocking {
        token("user-a", "device-1")
        session.signIn(alice)
        writeLocal(database, engine.handle, LocalChange.Setting("inbox_window_days", kotlinx.serialization.json.JsonPrimitive(7)))
        db.exec("insert into device_setting (key, value) values ('theme', '\"dark\"'), (?, '\"e1\"')", LAST_EPISODE_ID)

        token("user-b", "device-3")
        assertEquals(SignInResult.Ok(SignedIn.Switched), session.signIn(alice.copy(username = "bob")))
        // The new user authenticates before the old data goes, so a failed sign-in wipes nothing.
        assertEquals(listOf("/v1/auth/login", "/v1/auth/login", "/v1/auth/logout"), stub.paths)
        assertEquals(0, count("select count(*) as n from outbox"))
        assertEquals(0, count("select count(*) as n from setting"))
        assertEquals("the theme stays, the last episode goes", 1, count("select count(*) as n from device_setting"))
        assertEquals(Account("https://pod.example", "bob"), loadAccount(db))
    }

    @Test
    fun refusalsAreWordedForTheForm() = runBlocking {
        val cases = listOf(
            Triple(401, "invalid_credentials", SignInError.InvalidCredentials),
            Triple(409, "username_taken", SignInError.UsernameTaken),
            Triple(403, "registration_disabled", SignInError.RegistrationDisabled),
            Triple(400, "bad_request", SignInError.Rejected("Passwords need 8 characters.")),
            Triple(429, "rate_limited", SignInError.RateLimited),
            // An unknown code falls back to its status.
            Triple(401, "something_new", SignInError.InvalidCredentials),
            Triple(500, "internal", SignInError.Server(500)),
        )
        for ((status, code, expected) in cases) {
            refuse(status, code, "Passwords need 8 characters.")
            assertEquals("$status $code", SignInResult.Failed(expected), session.signIn(alice))
        }
        assertEquals(SignInResult.Failed(SignInError.InvalidServerUrl), session.signIn(alice.copy(serverUrl = "ftp://x")))
        assertNull(loadAccount(db))
    }

    @Test
    fun logoutKeepsDeviceSettingsAndTellsTheServer() = runBlocking {
        token("user-a", "device-1")
        session.signIn(alice)
        db.exec("insert into device_setting (key, value) values ('theme', '\"dark\"')")
        stub.status = 204
        stub.body = ""
        session.logout()
        assertNull(loadAccount(db))
        assertEquals(1, count("select count(*) as n from device_setting"))
        assertEquals("/v1/auth/logout", stub.paths.last())
    }
}
