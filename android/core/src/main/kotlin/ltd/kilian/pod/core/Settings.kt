package ltd.kilian.pod.core

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive

/**
 * Synced settings the clients understand, with their defaults (spec 2.4). The server fills these
 * for keys a user never set; clients use them before the first sync. Other keys are still stored
 * and synced unchanged, so older clients keep what newer ones write. Mirrors pod-protocol's
 * `settings.rs`.
 */
object Settings {
    const val INBOX_WINDOW_DAYS = "inbox_window_days"
    const val PLAYBACK_SPEED = "playback_speed"
    const val SKIP_BACK_MS = "skip_back_ms"
    const val SKIP_FORWARD_MS = "skip_forward_ms"
    const val PLAYED_THRESHOLD_MS = "played_threshold_ms"

    /** Every known key with its default, in the spec's order. */
    val DEFAULTS: Map<String, JsonPrimitive> = linkedMapOf(
        INBOX_WINDOW_DAYS to JsonPrimitive(DEFAULT_INBOX_WINDOW_DAYS),
        PLAYBACK_SPEED to JsonPrimitive(1.0),
        SKIP_BACK_MS to JsonPrimitive(15_000),
        SKIP_FORWARD_MS to JsonPrimitive(30_000),
        PLAYED_THRESHOLD_MS to JsonPrimitive(30_000),
    )

    const val DEFAULT_INBOX_WINDOW_DAYS = 30L

    /** Longest window honoured, so `now - window` stays a valid timestamp. A century covers every back catalogue. */
    const val MAX_INBOX_WINDOW_DAYS = 36_500L

    const val DAY_MS = 86_400_000L

    /**
     * The inbox window in days for the stored `inbox_window_days` value (spec 6). The spec types
     * it as an int; anything else (a fraction, a negative number, a string, null) is a client bug
     * that mustn't empty the inbox, so it reads as the default.
     */
    fun inboxWindowDays(stored: JsonElement?): Long {
        val days = (stored as? JsonPrimitive)?.takeUnless { it.isString }?.content?.toLongOrNull()
        return if (days == null || days < 0) DEFAULT_INBOX_WINDOW_DAYS else minOf(days, MAX_INBOX_WINDOW_DAYS)
    }
}
