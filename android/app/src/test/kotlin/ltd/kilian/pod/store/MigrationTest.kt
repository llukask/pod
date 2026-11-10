package ltd.kilian.pod.store

import android.content.Context
import android.database.sqlite.SQLiteDatabase
import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

// The schema steps, run on a database built from Room's exported schema of the version before.
// Opening it with the current `PodDatabase` runs the migrations and then Room's own check that
// the result matches the entities, so a column that differs in type or default fails here.

@RunWith(RobolectricTestRunner::class)
class MigrationTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val name = "migration-test.db"
    private var database: PodDatabase? = null

    @After
    fun close() {
        database?.close()
        context.deleteDatabase(name)
    }

    /** A database file at [version], made from `schemas/<version>.json`, with [seed] run on it. */
    private fun createAt(version: Int, seed: (SQLiteDatabase) -> Unit) {
        context.deleteDatabase(name)
        val dir = File(System.getProperty("pod.schemas") ?: error("pod.schemas is set by the Gradle test task"))
        val schema = Json.parseToJsonElement(File(dir, "$version.json").readText()).jsonObject.getValue("database").jsonObject
        val file = context.getDatabasePath(name).apply { parentFile?.mkdirs() }
        val sqlite = SQLiteDatabase.openOrCreateDatabase(file, null)
        for (entity in schema.getValue("entities").jsonArray.map { it.jsonObject }) {
            val table = entity.getValue("tableName").jsonPrimitive.content
            fun sql(of: JsonObject) = of.getValue("createSql").jsonPrimitive.content.replace("\${TABLE_NAME}", table)
            sqlite.execSQL(sql(entity))
            entity["indices"]?.jsonArray?.forEach { sqlite.execSQL(sql(it.jsonObject)) }
        }
        schema.getValue("setupQueries").jsonArray.forEach { sqlite.execSQL(it.jsonPrimitive.content) }
        seed(sqlite)
        sqlite.version = version
        sqlite.close()
    }

    private fun openCurrent(): PodDatabase =
        Room.databaseBuilder(context, PodDatabase::class.java, name)
            .addMigrations(MIGRATION_1_2)
            .allowMainThreadQueries()
            .build()
            .also { database = it }

    @Test
    fun version2AddsTheTypesAndPullsEveryPodcastScopeAgain() {
        createAt(1) { db ->
            db.execSQL("insert into podcast (id, feed_url, status, version) values ('p1', 'https://example.com/f', 'ok', 7)")
            db.execSQL(
                """insert into episode (id, podcast_id, title, summary_html, content_html, published_at, audio_url, audio_type, version)
                   values ('e1', 'p1', 'Trailer', '', '', 0, 'https://example.com/e1.mp3', 'audio/mpeg', 5)""",
            )
            db.execSQL("insert into sync_scope (scope, version, complete) values ('user', 40, 1)")
            db.execSQL("insert into sync_scope (scope, version, complete) values ('podcast:p1', 7, 1)")
        }

        val db = openCurrent().openHelper.readableDatabase

        assertEquals("episodic", db.queryOne("select show_type from podcast where id = 'p1'") { it.string("show_type") })
        assertEquals("full", db.queryOne("select episode_type from episode where id = 'e1'") { it.string("episode_type") })
        assertEquals(
            listOf("podcast:p1" to (0L to false), "user" to (40L to true)),
            db.queryList("select scope, version, complete from sync_scope order by scope") {
                it.string("scope") to (it.long("version") to it.flag("complete"))
            },
        )
    }
}
