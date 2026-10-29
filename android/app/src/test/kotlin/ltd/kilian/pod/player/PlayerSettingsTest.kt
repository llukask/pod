package ltd.kilian.pod.player

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import ltd.kilian.pod.core.Settings
import org.junit.Assert.assertEquals
import org.junit.Test

class PlayerSettingsTest {
    @Test
    fun missingSettingsReadAsTheSpecDefaults() {
        assertEquals(PlayerSettings(1.0, 15_000, 30_000, 30_000), PlayerSettings.fromValues(emptyMap()))
    }

    @Test
    fun storedSettingsApplyAndBadValuesFallBack() {
        val settings = PlayerSettings.fromValues(
            mapOf<String, JsonElement>(
                Settings.PLAYBACK_SPEED to JsonPrimitive(1.3),
                Settings.SKIP_BACK_MS to JsonPrimitive(10_000),
                Settings.SKIP_FORWARD_MS to JsonPrimitive("long"),
                Settings.PLAYED_THRESHOLD_MS to JsonPrimitive(-1),
            ),
        )
        assertEquals(PlayerSettings(1.3, 10_000, 30_000, 30_000), settings)
        assertEquals(1.0, PlayerSettings.fromValues(mapOf(Settings.PLAYBACK_SPEED to JsonPrimitive("fast"))).speed, 0.0)
        assertEquals(1.0, PlayerSettings.fromValues(mapOf(Settings.PLAYBACK_SPEED to JsonPrimitive(50))).speed, 0.0)
    }

    @Test
    fun stepsCycleUpwardsAndWrap() {
        assertEquals(1.1, nextStep(SPEEDS, 1.0), 0.0)
        assertEquals("between steps goes up", 1.2, nextStep(SPEEDS, 1.15), 0.0)
        assertEquals(0.8, nextStep(SPEEDS, 2.0), 0.0)
        assertEquals(5_000L, nextStep(SKIP_STEPS_MS, 60_000L))
    }
}
