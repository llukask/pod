package ltd.kilian.pod.player

import androidx.sqlite.db.SupportSQLiteDatabase
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.longOrNull
import ltd.kilian.pod.core.Settings
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.string

/** The speeds the player accepts from the setting. Anything outside a range a person would listen at is more likely a bad value than a wish. */
val SPEED_RANGE = 0.25..4.0

/** The speeds the speed control steps through. */
val SPEEDS = listOf(0.8, 1.0, 1.1, 1.2, 1.3, 1.5, 1.75, 2.0)

/** The skip intervals a long press on a skip button steps through. */
val SKIP_STEPS_MS = listOf(5_000L, 10_000L, 15_000L, 30_000L, 45_000L, 60_000L)

/** The synced settings the player applies (spec 2.4). Mirrors pod-desktop's `player/settings.rs`. */
data class PlayerSettings(
    val speed: Double = 1.0,
    val skipBackMs: Long = 15_000,
    val skipForwardMs: Long = 30_000,
    val playedThresholdMs: Long = 30_000,
) {
    companion object {
        /** Reads the settings by key. A missing value, or one of the wrong type or out of range, reads as the default, as spec 2.4 asks of clients. */
        fun fromValues(values: Map<String, JsonElement>): PlayerSettings {
            val defaults = PlayerSettings()
            fun number(key: String) = (values[key] as? JsonPrimitive)?.takeUnless { it.isString }
            fun millis(key: String, default: Long) = number(key)?.longOrNull?.takeIf { it >= 0 } ?: default
            return PlayerSettings(
                speed = number(Settings.PLAYBACK_SPEED)?.doubleOrNull?.takeIf { it in SPEED_RANGE } ?: defaults.speed,
                skipBackMs = millis(Settings.SKIP_BACK_MS, defaults.skipBackMs),
                skipForwardMs = millis(Settings.SKIP_FORWARD_MS, defaults.skipForwardMs),
                playedThresholdMs = millis(Settings.PLAYED_THRESHOLD_MS, defaults.playedThresholdMs),
            )
        }

        /** A value that isn't JSON is left out, so it reads as the default. */
        fun load(db: SupportSQLiteDatabase): PlayerSettings = fromValues(
            db.queryList("select key, value from setting") { it.string("key") to it.string("value") }
                .mapNotNull { (key, text) -> runCatching { key to WireJson.parseToJsonElement(text) }.getOrNull() }
                .toMap(),
        )
    }
}

/** The step after [current] in [steps], wrapping around. A value between steps moves to the next one up. */
fun <T : Comparable<T>> nextStep(steps: List<T>, current: T): T = steps.firstOrNull { it > current } ?: steps.first()
