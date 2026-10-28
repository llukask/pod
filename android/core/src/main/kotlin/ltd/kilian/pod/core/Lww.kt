package ltd.kilian.pod.core

import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/** The `(*_at, *_dev)` pair stamped on a field by the action that wrote it. `at` is epoch ms. */
data class Stamp(val at: Long, val dev: String) {
    companion object {
        /**
         * Reads a nullable column pair. Every write sets both, so a half-null pair can't be
         * produced by any rule and reads as unstamped, as in pod-protocol.
         */
        fun fromColumns(at: Long?, dev: String?): Stamp? =
            if (at != null && dev != null) Stamp(at, dev) else null
    }
}

/**
 * Whether a write stamped [new] replaces a field stamped [old] (`null` when never written),
 * spec 4.4. The later `at` wins and the greater device id breaks a tie; an exact tie from the
 * same device goes to the new write, because one device's mutations arrive in order.
 *
 * Device ids compare as their lowercase strings (spec 5.4), never through
 * `java.util.UUID.compareTo`, which compares signed 64-bit halves.
 */
fun wins(new: Stamp, old: Stamp?): Boolean =
    old == null || new.at > old.at || (new.at == old.at && new.dev >= old.dev)

/** RFC 3339 timestamps on the wire, epoch milliseconds in the local store. */
object Timestamps {
    /** Floors to the millisecond, which is the precision every stored timestamp has. */
    fun parse(text: String): Long = OffsetDateTime.parse(text).toInstant().toEpochMilli()

    private val MILLIS: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'").withZone(ZoneOffset.UTC)

    /** RFC 3339 in UTC with exactly three fraction digits, as the server and the fixtures write it. */
    fun format(epochMs: Long): String = MILLIS.format(Instant.ofEpochMilli(epochMs))
}
