package ltd.kilian.pod.settings

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import ltd.kilian.pod.core.Settings
import ltd.kilian.pod.downloads.DOWNLOAD_BUDGET_BYTES
import ltd.kilian.pod.downloads.DownloadSettings
import ltd.kilian.pod.downloads.EVICT_GRACE_HOURS
import ltd.kilian.pod.player.PlayerSettings
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class FieldsTest {
    /** What the synced readers make of [values], with default download settings. */
    private fun read(values: Map<String, JsonElement>) = Current(
        player = PlayerSettings.fromValues(values),
        inboxWindowDays = Settings.inboxWindowDays(values[Settings.INBOX_WINDOW_DAYS]),
        downloads = DownloadSettings(),
    )

    private fun field(key: String) = (SYNCED_FIELDS + DEVICE_FIELDS).single { it.key == key }

    @Test
    fun everySyncedStepReadsBackAsItself() {
        SYNCED_FIELDS.forEach { field ->
            field.steps.forEach { step ->
                assertEquals("${field.key} = $step", step, field.current(read(mapOf(field.key to step))))
            }
        }
    }

    /** The budget default is binary (2 GiB) and the steps decimal, so it is the one exception. */
    @Test
    fun theDefaultsAreSteps() {
        val defaults = read(emptyMap())
        (SYNCED_FIELDS + DEVICE_FIELDS).filter { it.key != DOWNLOAD_BUDGET_BYTES }.forEach { field ->
            assertTrue(field.key, field.current(defaults) in field.steps)
        }
    }

    @Test
    fun valuesAreWordedLikeTheWireframe() {
        val defaults = read(emptyMap())
        assertEquals(
            listOf("1×", "15 s", "30 s", "30 s", "30 days", "2.1 GB", "24 h"),
            (SYNCED_FIELDS + DEVICE_FIELDS).map { it.describe(defaults) },
        )
        assertEquals("Off", field(Settings.PLAYED_THRESHOLD_MS).label(JsonPrimitive(0)))
        assertEquals("2 min", field(Settings.PLAYED_THRESHOLD_MS).label(JsonPrimitive(120_000)))
        assertEquals("1 year", field(Settings.INBOX_WINDOW_DAYS).label(JsonPrimitive(365)))
        assertEquals("500.0 MB", field(DOWNLOAD_BUDGET_BYTES).label(JsonPrimitive(500_000_000)))
        assertEquals("1 week", field(EVICT_GRACE_HOURS).label(JsonPrimitive(168)))
    }
}
