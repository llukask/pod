package ltd.kilian.pod.downloads

import androidx.room.Room
import androidx.sqlite.db.SupportSQLiteDatabase
import androidx.test.core.app.ApplicationProvider
import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import ltd.kilian.pod.core.Timestamps
import ltd.kilian.pod.net.NetworkState
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.compareTables
import ltd.kilian.pod.store.dumpJsonRows
import ltd.kilian.pod.store.insertJsonRow
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.ParameterizedRobolectricTestRunner
import org.robolectric.ParameterizedRobolectricTestRunner.Parameters

/**
 * Runs `conformance/downloads.json` (spec 7.4, format in `conformance/README.md`) through
 * [load], [plan] and [apply] on the Room store: one pass under the case's conditions, then a
 * second pass that must plan nothing. A runner starts nothing for real; started rows stay
 * `transferring` under their new run.
 */
@RunWith(ParameterizedRobolectricTestRunner::class)
class DownloadsConformanceTest(private val name: String, private val case: JsonObject) {
    companion object {
        private val file = File(System.getProperty("pod.conformance") ?: error("pod.conformance is set by the Gradle test task"), "downloads.json")

        @JvmStatic
        @Parameters(name = "{0}")
        fun cases(): List<Array<Any>> =
            Json.parseToJsonElement(file.readText()).jsonObject.getValue("cases").jsonArray.map { case ->
                arrayOf(case.jsonObject.getValue("name").jsonPrimitive.content, case.jsonObject)
            }

        /** Columns the reconciler never reads, filled in when a fixture row leaves them out. */
        private val FILLER = mapOf(
            "episode" to mapOf(
                "title" to JsonPrimitive("An episode"), "summary_html" to JsonPrimitive(""), "content_html" to JsonPrimitive(""),
                "audio_type" to JsonPrimitive("audio/mpeg"), "version" to JsonPrimitive(1),
            ),
            "download" to mapOf("path" to JsonPrimitive("")),
        )
    }

    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java)
        .allowMainThreadQueries()
        .build()
    private val db: SupportSQLiteDatabase get() = database.openHelper.writableDatabase

    @After
    fun close() = database.close()

    private var runs = 0

    /** One pass: load, plan, apply. */
    private fun pass(conditions: Conditions): Pair<Plan, Applied> = database.runInTransaction<Pair<Plan, Applied>> {
        val plan = plan(load(db), conditions)
        plan to apply(db, plan.actions) { "0199b000-0000-7000-8000-%012d".format(++runs) }
    }

    @Test
    fun conforms() {
        case.getValue("before").jsonObject.forEach { (table, rows) ->
            rows.jsonArray.forEach { row -> insertJsonRow(db, table, JsonObject((FILLER[table] ?: emptyMap()) + row.jsonObject)) }
        }
        val given = case.getValue("conditions").jsonObject
        var conditions = Conditions(
            now = Timestamps.parse(given.getValue("now").jsonPrimitive.content),
            network = NetworkState(given.getValue("connected").jsonPrimitive.boolean, given.getValue("metered").jsonPrimitive.boolean),
            freeBytes = given.getValue("free_bytes").jsonPrimitive.long,
            playing = (given["playing"] as? JsonPrimitive)?.takeIf { it.isString }?.content,
            live = ids(given.getValue("live")).toSet(),
        )
        val problems = mutableListOf<String>()

        val (first, applied) = pass(conditions)
        problems += compareTables(dumpJsonRows(db), tables(case.getValue("after")))
        val started = applied.starts.map { it.episodeId }
        if (started != ids(case.getValue("started"))) problems += "started: want ${ids(case.getValue("started"))}, got $started"
        val skipped = ids(case.getValue("skipped")).toSet()
        if (first.fit.skipped != skipped) problems += "skipped: want $skipped, got ${first.fit.skipped}"

        // Between the passes every cancelled job has ended and every started one is running.
        conditions = conditions.copy(live = conditions.live - applied.cancels.toSet() + started)
        val (second, _) = pass(conditions)
        if (second.actions.isNotEmpty()) problems += "the second pass changes ${second.actions}"
        if (second.fit != first.fit) problems += "the fit moves from ${first.fit} to ${second.fit}"

        assertTrue(problems.joinToString("\n", prefix = "\n"), problems.isEmpty())
    }

    private fun ids(element: JsonElement): List<String> = element.jsonArray.map { it.jsonPrimitive.content }

    private fun tables(element: JsonElement): Map<String, List<JsonObject>> =
        element.jsonObject.mapValues { (_, rows) -> (rows as JsonArray).map { it.jsonObject } }
}
