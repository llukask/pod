package ltd.kilian.pod.sync

import androidx.room.Room
import androidx.sqlite.db.SupportSQLiteDatabase
import androidx.test.core.app.ApplicationProvider
import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import ltd.kilian.pod.core.Mutation
import ltd.kilian.pod.core.SyncRequest
import ltd.kilian.pod.core.SyncResponse
import ltd.kilian.pod.core.Timestamps
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.compareTables
import ltd.kilian.pod.store.dumpJsonRows
import ltd.kilian.pod.store.insertJsonRow
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.ParameterizedRobolectricTestRunner
import org.robolectric.ParameterizedRobolectricTestRunner.Parameters

/**
 * Runs the client state-transition fixtures (`conformance/sync_*.json` and `client_*.json`,
 * spec section 9) against the Room store, through the same functions the engine and the UI
 * call. The server is the fixture: a `send` step builds the request the engine would post and
 * marks its batch in flight, and the following `receive` applies the fixture's response.
 * `conformance/README.md` documents the format.
 */
@RunWith(ParameterizedRobolectricTestRunner::class)
class SyncConformanceTest(private val file: String, private val name: String, private val case: JsonObject) {
    companion object {
        private val dir = File(System.getProperty("pod.conformance") ?: error("pod.conformance is set by the Gradle test task"))

        @JvmStatic
        @Parameters(name = "{0}: {1}")
        fun cases(): List<Array<Any>> {
            val files = dir.listFiles { f -> f.name.matches(Regex("(sync|client)_.*\\.json")) }!!.sortedBy { it.name }
            check(files.any { it.name == "client_sync.json" }) { "found only $files" }
            return files.flatMap { f ->
                Json.parseToJsonElement(f.readText()).jsonObject.getValue("cases").jsonArray.map { c ->
                    arrayOf(f.name, c.jsonObject.getValue("name").jsonPrimitive.content, c.jsonObject)
                }
            }
        }
    }

    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java)
        .allowMainThreadQueries()
        .build()
    private val db: SupportSQLiteDatabase get() = database.openHelper.writableDatabase

    @After
    fun close() = database.close()

    @Test
    fun conforms() {
        val problems = mutableListOf<String>()
        tables(case.getValue("local_before")).forEach { (table, rows) -> rows.forEach { insertJsonRow(db, table, it) } }

        val steps: List<JsonObject> = when {
            "server_response" in case && "steps" !in case -> listOf(
                JsonObject(mapOf("step" to JsonPrimitive("send"))),
                JsonObject(mapOf("step" to JsonPrimitive("receive"), "response" to case.getValue("server_response"))),
            )
            "steps" in case && "server_response" !in case -> case.getValue("steps").jsonArray.map { it.jsonObject }
            else -> error("a case has either `server_response` or `steps`")
        }

        val inFlight = InFlight()
        // The user the last request was made for, until its response arrives.
        var sentFor: String? = null
        steps.forEachIndexed { index, step ->
            val at = "step ${index + 1}"
            database.runInTransaction {
                when (val kind = step.getValue("step").jsonPrimitive.content) {
                    "send" -> {
                        val prepared = prepare(db, pullOnly = false, inFlight) ?: error("$at: no account to send for")
                        val actual = WireJson.encodeToJsonElement(SyncRequest.serializer(), prepared.request).jsonObject
                        (step["request"] as? JsonObject)?.forEach { (key, want) ->
                            val got = actual[key] ?: JsonNull
                            if (got != want) problems += "$at: request.$key: want $want, got $got"
                        }
                        sentFor = prepared.userId
                    }
                    "receive" -> {
                        val userId = sentFor ?: error("$at: a receive needs a send before it")
                        sentFor = null
                        val response = WireJson.decodeFromJsonElement(SyncResponse.serializer(), step.getValue("response"))
                        val round = finishRound(db, inFlight, userId, response, pullOnly = false, now = System.currentTimeMillis())
                        val outcome = if (round == Round.More) "continue" else "stop"
                        (step["loop"] as? JsonPrimitive)?.content?.let { want ->
                            if (want != outcome) problems += "$at: loop: want $want, got $outcome"
                        }
                    }
                    "write" -> {
                        val now = Timestamps.parse(step.getValue("now").jsonPrimitive.content)
                        val (id, change) = localChange(step.getValue("mutation").jsonObject, now)
                        inFlight.withLock { ids -> applyLocalChange(db, change, id, ids, now) }
                    }
                    "logout" -> deleteAccountData(db)
                    "expect" -> problems += compareTables(dumpJsonRows(db), tables(step.getValue("tables"))).map { "$at: $it" }
                    else -> error("$at: unknown step $kind")
                }
            }
        }

        problems += compareTables(dumpJsonRows(db), tables(case.getValue("local_after")))
        assertTrue(problems.joinToString("\n", prefix = "\n"), problems.isEmpty())
    }

    private fun tables(element: JsonElement): Map<String, List<JsonObject>> =
        element.jsonObject.mapValues { (_, rows) -> (rows as JsonArray).map { it.jsonObject } }

    /**
     * The user action behind a `write` step's mutation, which comes without `at` and `dev`. An
     * episode action takes its podcast id from the episode's state row, else its episode row.
     */
    private fun localChange(mutation: JsonObject, now: Long): Pair<String, LocalChange> {
        val stamped = JsonObject(
            mutation + mapOf("at" to JsonPrimitive(Timestamps.format(now)), "dev" to JsonPrimitive("00000000-0000-0000-0000-000000000000"))
        )
        val parsed = WireJson.decodeFromJsonElement(Mutation.serializer(), stamped)
        fun podcastOf(episodeId: String): String =
            db.queryOne(
                "select podcast_id from episode_state where episode_id = ? union all select podcast_id from episode where id = ?",
                episodeId, episodeId,
            ) { it.string("podcast_id") } ?: error("episode $episodeId has no state or episode row")
        val change = when (parsed) {
            // Playback's periodic saves only differ in how soon they sync.
            is Mutation.EpisodePosition -> LocalChange.Episode(parsed.episodeId, podcastOf(parsed.episodeId), EpisodeChange.Position(parsed.positionMs, whilePlaying = false))
            is Mutation.EpisodePlayed -> LocalChange.Episode(parsed.episodeId, podcastOf(parsed.episodeId), EpisodeChange.Played(parsed.played))
            is Mutation.EpisodeArchived -> LocalChange.Episode(parsed.episodeId, podcastOf(parsed.episodeId), EpisodeChange.Archived(parsed.archived))
            is Mutation.SubscriptionSet -> LocalChange.Subscription(parsed.podcastId, parsed.feedUrl, parsed.subscribed)
            is Mutation.SettingSet -> LocalChange.Setting(parsed.key, parsed.value)
        }
        return parsed.id to change
    }
}
