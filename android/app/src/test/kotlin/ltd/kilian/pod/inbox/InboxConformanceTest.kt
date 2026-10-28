package ltd.kilian.pod.inbox

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import ltd.kilian.pod.core.Timestamps
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/** `conformance/inbox.json`: the spec 6 query on a fixed dataset, through the function the inbox screen reads. */
@RunWith(RobolectricTestRunner::class)
class InboxConformanceTest {
    private val database = Room.inMemoryDatabaseBuilder(ApplicationProvider.getApplicationContext(), PodDatabase::class.java)
        .allowMainThreadQueries()
        .build()
    private val db get() = database.openHelper.writableDatabase

    private val fixture: JsonObject = Json.parseToJsonElement(
        File(System.getProperty("pod.conformance") ?: error("pod.conformance is set by the Gradle test task"), "inbox.json").readText()
    ).jsonObject

    @After
    fun close() = database.close()

    @Test
    fun everyCasePasses() {
        seed()
        val cases = fixture.getValue("cases").jsonArray.map { it.jsonObject }
        check(cases.isNotEmpty())
        for (case in cases) {
            val name = case.getValue("name").jsonPrimitive.content
            db.exec("delete from setting")
            val days = case.getValue("inbox_window_days")
            if (days != JsonNull) {
                db.exec("insert into setting (key, value, value_at, value_dev) values ('inbox_window_days', ?, 0, 'x')", days.toString())
            }
            val now = Timestamps.parse(case.getValue("now").jsonPrimitive.content)
            val inbox = loadInbox(db, now)
            assertEquals(name, case.getValue("inbox").jsonArray.map { it.jsonPrimitive.content }, inbox.map { it.id })
        }
    }

    /**
     * The dataset lists only the columns the query reads; the other `NOT NULL` columns get filler.
     * Deleted episodes are tombstones a client has already removed, so they aren't seeded.
     */
    private fun seed() {
        fun JsonObject.text(key: String) = getValue(key).jsonPrimitive.content
        fun JsonObject.time(key: String) = Timestamps.parse(text(key))
        fun JsonObject.flag(key: String) = getValue(key).jsonPrimitive.boolean
        for (p in fixture.getValue("podcasts").jsonArray.map { it.jsonObject }) {
            db.exec("insert into podcast (id, feed_url, status, title, version) values (?, 'https://example.com/feed', 'ok', ?, 1)", p.text("id"), p.text("title"))
        }
        for (s in fixture.getValue("subscriptions").jsonArray.map { it.jsonObject }) {
            db.exec(
                "insert into subscription (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev) values (?, 'https://example.com/feed', ?, ?, 'x')",
                s.text("podcast_id"), s.flag("subscribed"), s.time("subscribed_at"),
            )
        }
        for (e in fixture.getValue("episodes").jsonArray.map { it.jsonObject }.filterNot { it.flag("deleted") }) {
            db.exec(
                """insert into episode (id, podcast_id, title, summary_html, content_html, published_at, audio_url, audio_type, version)
                   values (?, ?, ?, '', '', ?, 'https://example.com/a.mp3', 'audio/mpeg', 1)""",
                e.text("id"), e.text("podcast_id"), e.text("title"), e.time("published_at"),
            )
        }
        for (st in fixture.getValue("episode_states").jsonArray.map { it.jsonObject }) {
            db.exec(
                "insert into episode_state (episode_id, podcast_id, position_ms, played, archived) values (?, ?, ?, ?, ?)",
                st.text("episode_id"), st.text("podcast_id"), st.getValue("position_ms").jsonPrimitive.content.toLong(), st.flag("played"), st.flag("archived"),
            )
        }
    }
}
