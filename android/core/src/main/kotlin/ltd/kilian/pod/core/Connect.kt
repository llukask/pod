package ltd.kilian.pod.core

import java.io.ByteArrayOutputStream

// ==============================================================================================
// Connect (spec 14)
// ==============================================================================================
//
// The rules a client applies around the event stream, kept pure so they can be tested without a
// server, a clock or a player: reading the stream, the reconnect delay, who plays, where a remote
// playback stands now, what a command shows before the active device confirms it, and what the
// local player does with an event.

/** The first reconnect waits this long; each further one doubles it up to [CONNECT_RECONNECT_MAX_MS] (14.2). */
const val CONNECT_RECONNECT_FIRST_MS = 1_000L
const val CONNECT_RECONNECT_MAX_MS = 30_000L

/** How often the playing device reports while it plays (14.3). */
const val CONNECT_REPORT_EVERY_MS = 15_000L

/**
 * The server sends a ping every 15 seconds (14.2), so a stream silent for three of them is dead,
 * the way a phone that changed networks leaves a socket that never errors.
 */
const val CONNECT_SILENCE_LIMIT_MS = 45_000L

/**
 * How long after the user moves the volume the server's volume is ignored. A state sent before
 * the change landed would otherwise jump the slider back for a moment.
 */
const val CONNECT_VOLUME_HOLD_MS = 3_000L

/** The delays between reconnects of a closed stream: 1 s, doubling to 30 s, from the start again once a `state` arrives. */
class ReconnectDelay {
    private var upcoming = CONNECT_RECONNECT_FIRST_MS

    /** The delay before the next attempt. */
    fun next(): Long = upcoming.also { upcoming = minOf(it * 2, CONNECT_RECONNECT_MAX_MS) }

    fun reset() {
        upcoming = CONNECT_RECONNECT_FIRST_MS
    }
}

/**
 * Reads a `text/event-stream` body as it arrives, returning the `data` of each event. Lines may
 * end in LF, CRLF or CR, and a chunk may end anywhere, even inside a UTF-8 character, since lines
 * are decoded only once complete. Comments (`: ping`) and the fields Connect doesn't use (`event`,
 * `id`, `retry`) are skipped. Several `data` lines of one event join with a newline, as the
 * format says, though the server sends one per event.
 */
class SseParser {
    private val line = ByteArrayOutputStream()
    private var data: StringBuilder? = null
    private var afterCr = false

    fun feed(chunk: ByteArray, length: Int = chunk.size): List<String> {
        val events = mutableListOf<String>()
        for (i in 0 until length) {
            val byte = chunk[i]
            if (afterCr && byte == LF) {
                afterCr = false
                continue
            }
            afterCr = byte == CR
            if (byte == LF || byte == CR) endLine(events) else line.write(byte.toInt())
        }
        return events
    }

    fun feed(text: String): List<String> = feed(text.encodeToByteArray())

    private fun endLine(events: MutableList<String>) {
        val text = line.toByteArray().decodeToString()
        line.reset()
        if (text.isEmpty()) {
            data?.let { events += it.toString() }
            data = null
            return
        }
        if (text.startsWith(":")) return
        val field = text.substringBefore(':')
        if (field != "data") return
        val value = if (':' in text) text.substringAfter(':').removePrefix(" ") else ""
        data = data?.append('\n')?.append(value) ?: StringBuilder(value)
    }

    private companion object {
        const val LF = '\n'.code.toByte()
        const val CR = '\r'.code.toByte()
    }
}

/** Decodes one event's `data`, or null for one this client doesn't know, so a later server's new event type is skipped. */
fun decodeConnectEvent(data: String): ConnectEvent? = try {
    WireJson.decodeFromString(ConnectEvent.serializer(), data)
} catch (e: IllegalArgumentException) {
    // SerializationException is an IllegalArgumentException.
    null
}

/** Who plays the account's playback, seen from one device. */
enum class ConnectRole {
    /** This device is the active one. */
    Active,

    /** Another client device or a Sonos group is active, and this device is its remote. */
    Remote,

    /** Nothing is active; the playback is the account's last one, paused. */
    Idle,
}

/** The role of the device [selfId] in [playback] (14.3): a remote exactly when the active device is set and isn't this one. */
fun connectRole(playback: Playback, selfId: String): ConnectRole = when (playback.deviceId) {
    null -> ConnectRole.Idle
    selfId -> ConnectRole.Active
    else -> ConnectRole.Remote
}

private val UUID_PATTERN = Regex("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")

/** Whether an active device id names a Sonos group: client devices are UUIDs, groups `RINCON_…` (14.1). */
fun isSonosDevice(deviceId: String): Boolean = !UUID_PATTERN.matches(deviceId)

/**
 * Where a playback seen at [positionMs] at [seenAt] stands at [now]: while it plays, the position
 * runs on by the elapsed time times [speed] (14.1), up to [durationMs] when that is known. A
 * position the server already reports past the duration stands, since feed durations are
 * approximate (13.3).
 */
fun playbackPositionAt(positionMs: Long, seenAt: Long, playing: Boolean, speed: Double, now: Long, durationMs: Long?): Long {
    if (!playing) return positionMs
    val running = positionMs + (maxOf(0, now - seenAt) * speed).toLong()
    return if (durationMs != null) minOf(running, maxOf(durationMs, positionMs)) else running
}

/**
 * Whether a remote's playback just ended (14.7, "Remote ended"): this device was a remote, and now
 * nothing is active. Its final position and played mark come with a sync.
 */
fun remoteEnded(wasRemote: Boolean, playback: Playback?): Boolean = wasRemote && playback != null && playback.deviceId == null

/** Whether the server's volume replaces the shown one, when the user last moved it at [touchedAt] (null: never). */
fun adoptsServerVolume(touchedAt: Long?, now: Long): Boolean = touchedAt == null || now - touchedAt >= CONNECT_VOLUME_HOLD_MS

/**
 * The playback as a remote shows it right after sending [command], before the active device
 * confirms: [positionMs] is where the shown playback stands now. `play` changes nothing yet,
 * since only the active device knows where the other episode starts.
 */
fun Playback.after(command: ConnectCommand, positionMs: Long): Playback {
    val duration = episode.durationMs
    fun clamp(ms: Long) = maxOf(0, if (duration != null) minOf(ms, duration) else ms)
    return when (command) {
        ConnectCommand.Pause -> copy(playing = false, positionMs = positionMs)
        ConnectCommand.Resume -> copy(playing = true, positionMs = positionMs)
        is ConnectCommand.Seek -> copy(positionMs = clamp(command.positionMs))
        is ConnectCommand.Skip -> copy(positionMs = clamp(positionMs + command.deltaMs))
        is ConnectCommand.Play -> copy(positionMs = positionMs)
        is ConnectCommand.Volume -> copy(positionMs = positionMs, volume = command.volume.coerceIn(0, 100))
    }
}

/** What the local player does for an event addressed to it (14.2), as if the user had pressed the button. */
sealed interface PlayerAction {
    data object Pause : PlayerAction

    data object Resume : PlayerAction

    data class SeekTo(val positionMs: Long) : PlayerAction

    /** Seek relative to the local position, clamped to the episode. */
    data class SkipBy(val deltaMs: Long) : PlayerAction

    /** Start [episodeId] by the start rule of spec 8, a replay included, then report. */
    data class Start(val episodeId: String) : PlayerAction

    /** Start [episodeId] at [positionMs], which isn't a replay, then report. */
    data class StartAt(val episodeId: String, val positionMs: Long) : PlayerAction

    /** Pause, report with `released` = [transferId], and follow the playback as a remote. */
    data class Release(val transferId: String?) : PlayerAction
}

/** The player's action for [event], or null for an event the controller handles itself (`state`, `replaced`) or one for Sonos only. */
fun playerAction(event: ConnectEvent): PlayerAction? = when (event) {
    is ConnectEvent.Command -> when (val command = event.command) {
        ConnectCommand.Pause -> PlayerAction.Pause
        ConnectCommand.Resume -> PlayerAction.Resume
        is ConnectCommand.Seek -> PlayerAction.SeekTo(maxOf(0, command.positionMs))
        is ConnectCommand.Skip -> PlayerAction.SkipBy(command.deltaMs)
        is ConnectCommand.Play -> PlayerAction.Start(command.episodeId)
        is ConnectCommand.Volume -> null
    }
    is ConnectEvent.Load -> event.positionMs?.let { PlayerAction.StartAt(event.episodeId, maxOf(0, it)) } ?: PlayerAction.Start(event.episodeId)
    is ConnectEvent.Release -> PlayerAction.Release(event.transferId)
    is ConnectEvent.State, ConnectEvent.Replaced -> null
}

/**
 * Whether the local player reports a change it didn't make on the user's behalf: the 15 s report,
 * a speed set on another device, a reconnected stream. Only while it plays or is the active
 * device; anything else would claim a playback nobody asked for, since a paused report claims
 * when nothing is active (14.1).
 */
fun reportsUnasked(playingLocally: Boolean, role: ConnectRole?): Boolean = playingLocally || role == ConnectRole.Active
