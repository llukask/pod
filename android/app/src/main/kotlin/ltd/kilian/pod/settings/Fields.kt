package ltd.kilian.pod.settings

import androidx.sqlite.db.SupportSQLiteDatabase
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonPrimitive
import ltd.kilian.pod.core.Settings
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.downloads.DOWNLOAD_BUDGET_BYTES
import ltd.kilian.pod.downloads.DownloadSettings
import ltd.kilian.pod.downloads.EVICT_GRACE_HOURS
import ltd.kilian.pod.downloads.formatBytes
import ltd.kilian.pod.downloads.loadSettings
import ltd.kilian.pod.player.PlayerSettings
import ltd.kilian.pod.player.SKIP_STEPS_MS
import ltd.kilian.pod.player.SPEEDS
import ltd.kilian.pod.player.skipLabel
import ltd.kilian.pod.player.speedLabel
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.sync.LocalChange
import ltd.kilian.pod.sync.SyncHandle
import ltd.kilian.pod.sync.writeLocal

// ==============================================================================================
// The settings the user picks from steps (wireframe M10)
// ==============================================================================================
//
// One table drives the settings screen's pickers. Each field names its key, the steps offered
// and how a value reads, and takes its current value from the same readers the player, the inbox
// and the reconciler use, so the screen shows what they apply (a stored value they reject shows
// as the default they fall back to). Every step lies inside its reader's accepted range; the
// tests write each one and read it back. Synced keys are saved through a local write, device
// keys as a plain `device_setting` row (`save` in the settings UI).

/** The settings as their readers see them. */
data class Current(val player: PlayerSettings, val inboxWindowDays: Long, val downloads: DownloadSettings)

/** One setting picked from [steps]. */
data class Field(
    val key: String,
    val title: String,
    /** Explains a setting whose title can't, under the title. */
    val note: String? = null,
    val steps: List<JsonPrimitive>,
    val label: (JsonPrimitive) -> String,
    val current: (Current) -> JsonPrimitive,
) {
    fun describe(current: Current): String = label(current(current))
}

/** Synced settings (spec 2.4): they follow the account to every device. */
val SYNCED_FIELDS: List<Field> = listOf(
    Field(
        key = Settings.PLAYBACK_SPEED,
        title = "Speed",
        steps = SPEEDS.map(::JsonPrimitive),
        label = { speedLabel(it.content.toFloat()) },
        current = { JsonPrimitive(it.player.speed) },
    ),
    Field(
        key = Settings.SKIP_BACK_MS,
        title = "Skip back",
        steps = SKIP_STEPS_MS.map(::JsonPrimitive),
        label = { skipLabel(it.content.toLong()) },
        current = { JsonPrimitive(it.player.skipBackMs) },
    ),
    Field(
        key = Settings.SKIP_FORWARD_MS,
        title = "Skip forward",
        steps = SKIP_STEPS_MS.map(::JsonPrimitive),
        label = { skipLabel(it.content.toLong()) },
        current = { JsonPrimitive(it.player.skipForwardMs) },
    ),
    Field(
        key = Settings.PLAYED_THRESHOLD_MS,
        title = "Mark played near the end",
        note = "Within this of the end, or the last 2 % if that's shorter",
        steps = listOf(0L, 5_000L, 10_000L, 15_000L, 30_000L, 60_000L, 120_000L).map(::JsonPrimitive),
        label = { val ms = it.content.toLong(); if (ms == 0L) "Off" else skipLabel(ms) },
        current = { JsonPrimitive(it.player.playedThresholdMs) },
    ),
    Field(
        key = Settings.INBOX_WINDOW_DAYS,
        title = "Inbox shows the last",
        steps = listOf(1L, 3L, 7L, 14L, 30L, 60L, 90L, 180L, 365L).map(::JsonPrimitive),
        label = { daysLabel(it.content.toLong()) },
        current = { JsonPrimitive(it.inboxWindowDays) },
    ),
)

/**
 * This device's download settings (spec 7.1). "Download on mobile data" is a switch, not a
 * picker, so it isn't here.
 */
val DEVICE_FIELDS: List<Field> = listOf(
    Field(
        key = DOWNLOAD_BUDGET_BYTES,
        title = "Storage budget",
        // Decimal gigabytes, the unit the labels and Android's storage settings use. The 2 GiB
        // default isn't one of them, so it reads "2.1 GB" until a step is picked.
        steps = listOf(0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0).map { JsonPrimitive((it * 1e9).toLong()) },
        label = { formatBytes(it.content.toLong()) },
        current = { JsonPrimitive(it.downloads.budget) },
    ),
    Field(
        key = EVICT_GRACE_HOURS,
        title = "Keep removed downloads for up to",
        note = "Played, archived and unpinned files, unless the space is needed sooner",
        steps = listOf(0L, 1L, 6L, 12L, 24L, 48L, 72L, 168L).map(::JsonPrimitive),
        label = { hoursLabel(it.content.toDouble()) },
        current = { JsonPrimitive(it.downloads.graceMs / 3_600_000) },
    ),
)

private fun daysLabel(days: Long): String = when (days) {
    1L -> "1 day"
    7L -> "1 week"
    14L -> "2 weeks"
    365L -> "1 year"
    else -> "$days days"
}

private fun hoursLabel(hours: Double): String = when (hours) {
    0.0 -> "Remove at once"
    168.0 -> "1 week"
    else -> "%.0f h".format(hours)
}

// ==============================================================================================
// Reading and saving
// ==============================================================================================

/** The tables [loadCurrent] reads. */
val SETTINGS_TABLES = arrayOf("setting", "device_setting")

fun loadCurrent(db: SupportSQLiteDatabase): Current {
    val window = db.queryOne("select value from setting where key = ?", Settings.INBOX_WINDOW_DAYS) { it.string("value") }
    return Current(
        player = PlayerSettings.load(db),
        inboxWindowDays = Settings.inboxWindowDays(window?.let { runCatching { WireJson.parseToJsonElement(it) }.getOrNull() }),
        downloads = loadSettings(db),
    )
}

/**
 * Saves [value] for [key]: a synced key as a local write, which reaches every device, a device
 * key as a plain `device_setting` row, which never leaves this one. The reconciler watches
 * `device_setting`, so a new budget or grace period applies on its next pass.
 */
suspend fun save(database: PodDatabase, sync: SyncHandle, key: String, value: JsonPrimitive) {
    if (SYNCED_FIELDS.any { it.key == key }) {
        writeLocal(database, sync, LocalChange.Setting(key, value))
    } else {
        withContext(Dispatchers.IO) {
            database.openHelper.writableDatabase.exec(
                "insert into device_setting (key, value) values (?, ?) on conflict (key) do update set value = excluded.value",
                key, WireJson.encodeToString(JsonPrimitive.serializer(), value),
            )
        }
    }
}
