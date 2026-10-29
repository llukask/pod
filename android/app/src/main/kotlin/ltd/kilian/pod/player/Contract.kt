package ltd.kilian.pod.player

// The playback contract of spec 8 as a pure state machine: what the player reports in (progress,
// pause, seek, stop, the end of the file) and the `episode_state` writes that follow. The clock
// comes in with every call, so tests drive it with made-up instants and no player. A port of
// pod-desktop's `player/contract.rs`.
//
// One [Contract] covers one play of one episode, from the moment it starts until it stops,
// changes or ends.

/** How often a playing episode saves its position (spec 8). */
const val SAVE_EVERY_MS = 5_000L

/** A write to the playing episode's state, made through the local writes API (spec 5.2) by the caller. */
sealed interface Write {
    /** [whilePlaying] marks the periodic saves, which may wait for a sync. */
    data class Position(val positionMs: Long, val whilePlaying: Boolean) : Write

    data class Played(val played: Boolean) : Write
}

/** The episode's state as stored when playback starts. */
data class Stored(val positionMs: Long, val played: Boolean) {
    /** Where playback starts: the stored position, or the beginning of a played episode. */
    val startPositionMs: Long get() = if (played) 0 else positionMs
}

class Contract private constructor(
    /** `played_threshold_ms` from the synced settings. */
    private val thresholdMs: Long,
    durationMs: Long?,
    positionMs: Long,
    /** When the last position write was made, or playback last resumed. The next periodic save is due [SAVE_EVERY_MS] after it. */
    private var savedAt: Long,
) {
    var durationMs: Long? = durationMs
        private set
    var positionMs: Long = positionMs
        private set
    var isPlaying: Boolean = true
        private set

    /** Whether this play wrote `played = true`, which it does once. */
    private var markedPlayed = false

    companion object {
        /**
         * Starts playing at [Stored.startPositionMs]. Replaying a played episode writes
         * `played = false` as its own mutation, so coalescing position saves can never lose the
         * reopen.
         */
        fun start(stored: Stored, durationMs: Long?, thresholdMs: Long, now: Long): Pair<Contract, List<Write>> =
            Contract(thresholdMs, durationMs, stored.startPositionMs, now) to
                if (stored.played) listOf(Write.Played(false)) else emptyList()
    }

    /** The player's own duration, which beats the feed's. */
    fun setDuration(durationMs: Long) {
        this.durationMs = durationMs
    }

    /** The player reports where it is. Saves the position every [SAVE_EVERY_MS] while playing. */
    fun progress(positionMs: Long, now: Long): List<Write> {
        this.positionMs = positionMs
        val writes = checkPlayed()
        if (isPlaying && now - savedAt >= SAVE_EVERY_MS) writes += save(true, now)
        return writes
    }

    fun pause(now: Long): List<Write> {
        if (!isPlaying) return emptyList()
        isPlaying = false
        return listOf(save(false, now))
    }

    fun resume(now: Long) {
        if (!isPlaying) {
            isPlaying = true
            savedAt = now
        }
    }

    fun seek(positionMs: Long, now: Long): List<Write> {
        this.positionMs = maxOf(positionMs, 0)
        return checkPlayed().apply { add(save(false, now)) }
    }

    /** Stop, episode change or service destroy: the last position write of this play. */
    fun stop(now: Long): List<Write> = listOf(save(false, now))

    /** The player reached the end of the file. Marks the episode played if the threshold didn't, which happens without a known duration. */
    fun ended(now: Long): List<Write> {
        durationMs?.let { positionMs = maxOf(positionMs, it) }
        val writes = mutableListOf<Write>()
        if (!markedPlayed) writes += Write.Played(true)
        writes += save(false, now)
        return writes
    }

    private fun save(whilePlaying: Boolean, now: Long): Write {
        savedAt = now
        return Write.Position(positionMs, whilePlaying)
    }

    /** `played = true`, once, when the remaining time drops below `played_threshold_ms` or 2 % of the duration, whichever is smaller. */
    private fun checkPlayed(): MutableList<Write> {
        val duration = durationMs ?: return mutableListOf()
        val limit = minOf(thresholdMs, duration / 50)
        if (markedPlayed || duration - positionMs >= limit) return mutableListOf()
        markedPlayed = true
        return mutableListOf(Write.Played(true))
    }
}
