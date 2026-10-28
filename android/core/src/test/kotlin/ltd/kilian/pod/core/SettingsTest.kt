package ltd.kilian.pod.core

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class SettingsTest {
    private fun days(json: String?) = Settings.inboxWindowDays(json?.let(Json::parseToJsonElement))

    @Test
    fun anUsableWindowIsTakenAsStored() {
        assertEquals(7, days("7"))
        assertEquals(0, days("0"))
    }

    @Test
    fun anUnusableWindowReadsAsTheDefault() {
        for (stored in listOf(null, "null", "-1", "1.5", "\"7\"", "true", "[]")) assertEquals(30, days(stored), stored)
        assertEquals(30, Settings.inboxWindowDays(JsonNull))
    }

    @Test
    fun aWindowBeyondACenturyIsCapped() {
        assertEquals(36_500, days("1000000"))
    }
}
