package ltd.kilian.pod.sonos

import android.util.Log
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import ltd.kilian.pod.core.SONOS_POLL_BACKGROUND_MS
import ltd.kilian.pod.core.SONOS_POLL_FOREGROUND_MS
import ltd.kilian.pod.core.SonosCommand
import ltd.kilian.pod.core.SonosObservation
import ltd.kilian.pod.core.SonosPlayer
import ltd.kilian.pod.core.SonosSessionView
import ltd.kilian.pod.core.SonosStep
import ltd.kilian.pod.core.adoptsPolledVolume
import ltd.kilian.pod.core.observeSonos
import ltd.kilian.pod.core.sonosPositionAt
import ltd.kilian.pod.core.stepSonos
import ltd.kilian.pod.sync.monotonicMs

// ==============================================================================================
// Sonos groups as an output of the player (spec 13.4)
// ==============================================================================================
//
// pod-server plays a handed-off episode on the group and writes its listening state (13.3).
// Meanwhile the playback service holds the episode parked and writes nothing, and this
// controller is the player: it starts, steers and releases the session, and polls the player
// list to show it. The service still does everything that touches the local player (the pause
// write and replay reset before a hand-off, continuing locally after a release), so it calls
// [start] and [release]; the UI calls the commands through `PlayerConnection`.
//
// All state lives on one serial dispatcher. A request suspends there, so another command can run
// meanwhile; each one checks afterwards that the [Output] it started under is still the current
// one (by identity), as the web client's `player.ts` does.

/** A Sonos group to play on. */
data class SonosTarget(val id: String, val name: String) {
    companion object {
        fun of(player: SonosPlayer) = SonosTarget(player.id, player.name)
    }
}

/**
 * Where the episode plays. [Connecting] and [Sonos] are compared by identity: each hand-off makes
 * a new one, so a request that outlives its mode can tell.
 */
sealed interface Output {
    data object Local : Output

    /** The play request to [target] is in flight; it can take half a minute on slow podcast hosts. */
    class Connecting(val target: SonosTarget, val episodeId: String) : Output {
        override fun toString() = "Connecting(${target.name}, $episodeId)"
    }

    /** The user's session on [target] plays [episodeId]. */
    class Sonos(val target: SonosTarget, val episodeId: String) : Output {
        override fun toString() = "Sonos(${target.name}, $episodeId)"
    }
}

/** The session as last seen: playing or not, at [positionMs] at the [monotonicMs] instant [seenAt]. */
data class SeenSession(val playing: Boolean, val positionMs: Long, val seenAt: Long) {
    fun positionAt(now: Long, durationMs: Long?) = sonosPositionAt(positionMs, seenAt, playing, now, durationMs)
}

data class SonosState(
    val output: Output = Output.Local,
    /** The session as shown, while [output] isn't [Output.Local]. */
    val session: SeenSession? = null,
    /** The group volume, 0 to 100, while a group has the episode and its volume is known. */
    val volume: Int? = null,
    /** Whether the server listed any group at the last fetch. The output picker shows only when it did. */
    val available: Boolean = false,
)

/** Where a released session stood, for the local player to go on from. */
data class Released(val episodeId: String, val positionMs: Long, val playing: Boolean)

class SonosController(
    private val api: SonosApi,
    /** Lands the player's queued writes and pushes the outbox, so the server starts from them. */
    private val flushWrites: suspend () -> Unit,
    /** Asks the sync engine for a sync now (spec 5.6). */
    private val requestSync: () -> Unit,
    /** Whether the app is in the foreground, which sets how often remote mode polls. */
    private val foreground: StateFlow<Boolean>,
    private val scope: CoroutineScope,
    private val clock: () -> Long = ::monotonicMs,
) {
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private val serial = Dispatchers.Default.limitedParallelism(1)

    private val published = MutableStateFlow(SonosState())
    val state: StateFlow<SonosState> = published.asStateFlow()

    private val told = MutableSharedFlow<String>(extraBufferCapacity = 8)

    /** Failures to show in a snackbar. */
    val messages: SharedFlow<String> = told.asSharedFlow()

    private val output get() = published.value.output

    /** The polling of the current session, while there is one. */
    private var polling: Job? = null

    /** Counts commands, so a poll sent before one doesn't undo what the UI already shows. */
    private var commands = 0

    /** When the group first went missing from the list, or null while it is there. */
    private var missingSince: Long? = null

    /** When the user last moved the volume, or null when a poll may set it. */
    private var volumeTouchedAt: Long? = null

    /**
     * The transport commands to send, one at a time in the order given, so a quick pause and
     * resume reach the group in that order.
     */
    private val transport = Channel<suspend () -> Unit>(Channel.UNLIMITED)

    /**
     * The volume changes to send. Conflated, so while one request is out only the newest value
     * waits: a dragged slider sends the value it ends on, not a request for every step.
     */
    private val volumes = Channel<Pair<Output.Sonos, Int>>(Channel.CONFLATED)

    init {
        scope.launch(serial) {
            for (send in transport) send()
        }
        scope.launch(serial) {
            for ((out, level) in volumes) sendVolume(out, level)
        }
    }

    private fun onSerial(block: suspend () -> Unit) {
        scope.launch(serial) { block() }
    }

    // ==========================================================================================
    // The player list
    // ==========================================================================================

    /** Every group, keeping [SonosState.available] current. Throws [SonosException]. */
    suspend fun fetchPlayers(): List<SonosPlayer> {
        val players = api.players()
        published.update { it.copy(available = players.isNotEmpty()) }
        return players
    }

    /** [fetchPlayers], or null when the server can't be asked. */
    suspend fun refresh(): List<SonosPlayer>? = try {
        fetchPlayers()
    } catch (e: SonosException) {
        null
    }

    // ==========================================================================================
    // Handing off and taking back
    // ==========================================================================================

    /**
     * Starts [episodeId] on [target] at [startMs]. The playback service calls this once it has
     * paused the local play and written the replay reset, so it only pushes those writes and
     * then asks the server. On failure the episode stays here, parked where it would have started.
     */
    fun start(target: SonosTarget, episodeId: String, startMs: Long) = onSerial {
        val connecting = Output.Connecting(target, episodeId)
        stopPolling()
        published.update { it.copy(output = connecting, session = SeenSession(false, startMs, clock()), volume = null) }
        try {
            try {
                flushWrites()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                // The start position goes in the request, so the hand-off doesn't need the push.
                Log.w(TAG, "pushing the outbox before a hand-off failed", e)
            }
            val player = api.play(target.id, episodeId, startMs)
            if (output !== connecting) return@onSerial
            val seen = observeSonos(listOf(player), episodeId, target.id)
            // The group was just told to play and may still be buffering, which the answer
            // reports as not playing; the polls take over from here.
            enterRemote(target, episodeId, (seen as? SonosObservation.Session)?.session?.positionMs ?: startMs, playing = true)
            followVolume(target, listOf(player))
        } catch (e: SonosException) {
            if (output !== connecting) return@onSerial
            // TODO: when another episode was playing on this group, its session goes on there
            // and this device no longer follows it.
            published.update { it.copy(output = Output.Local, session = null, volume = null) }
            told.tryEmit(e.describe(target.name))
        }
    }

    /** Moves the session to another group: released here, started there where it stood. */
    fun switchTo(target: SonosTarget) = onSerial {
        val out = output as? Output.Sonos ?: return@onSerial
        if (out.target.id == target.id) return@onSerial
        val released = releaseFrom(out) ?: return@onSerial
        start(target, released.episodeId, released.positionMs)
    }

    /**
     * Ends the session and leaves remote mode. Returns where the episode stood and whether it
     * played, for the local player to go on from, or null when the group couldn't be reached and
     * the session goes on.
     */
    suspend fun release(): Released? = withContext(serial) {
        (output as? Output.Sonos)?.let { releaseFrom(it) }
    }

    private suspend fun releaseFrom(out: Output.Sonos): Released? {
        val shown = shown()
        var atMs = shown.positionMs
        var wasPlaying = shown.playing
        try {
            val released = api.release(out.target.id)
            if (released.episodeId == out.episodeId) {
                atMs = released.positionMs
                wasPlaying = released.playing
            }
        } catch (e: SonosException) {
            if (output !== out) return null
            if (!e.sessionGone) {
                told.tryEmit(e.describe(out.target.name))
                return null
            }
            // Over already, so nothing plays on that this device could take over. Its final
            // state comes with the next sync.
            wasPlaying = false
            requestSync()
        }
        if (output !== out) return null
        leaveRemote()
        return Released(out.episodeId, atMs, wasPlaying)
    }

    /**
     * Follows a session that played on while this process wasn't running. The playback service
     * finds it with [refresh] and [observeSonos] after restoring the episode, and calls this while
     * nothing plays here.
     */
    fun follow(player: SonosPlayer, session: SonosSessionView) = onSerial {
        if (output != Output.Local) return@onSerial
        enterRemote(SonosTarget.of(player), session.episodeId, session.positionMs, session.playing)
        followVolume(SonosTarget.of(player), listOf(player))
    }

    /** Forgets the session after logout. The group plays on; the next account can't steer it. */
    fun signedOut() = onSerial {
        leaveRemote()
        published.update { it.copy(available = false) }
    }

    // ==========================================================================================
    // Commands
    // ==========================================================================================

    fun toggle() = onSerial {
        val out = output as? Output.Sonos ?: return@onSerial
        val before = shown()
        setShown(!before.playing, before.positionMs)
        send(out, if (before.playing) SonosCommand.Pause else SonosCommand.Resume, before)
    }

    /** Seeks to [toMs], which the caller clamps to the episode. */
    fun seek(toMs: Long) = onSerial {
        val out = output as? Output.Sonos ?: return@onSerial
        val before = shown()
        val target = maxOf(0, toMs)
        setShown(before.playing, target)
        send(out, SonosCommand.Seek(target), before)
    }

    /**
     * Skips by [deltaMs], the synced `skip_back_ms` negated or `skip_forward_ms`. The group skips
     * from its own position; the clock between polls stands in for it here, up to [durationMs].
     */
    fun skip(deltaMs: Long, durationMs: Long?) = onSerial {
        val out = output as? Output.Sonos ?: return@onSerial
        val before = shown()
        val moved = maxOf(0, before.positionMs + deltaMs)
        setShown(before.playing, if (durationMs != null) minOf(moved, durationMs) else moved)
        send(out, SonosCommand.Skip(deltaMs), before)
    }

    /** Sets the group volume, 0 to 100. */
    fun setVolume(level: Int) = onSerial {
        val out = output as? Output.Sonos ?: return@onSerial
        val clamped = level.coerceIn(0, 100)
        published.update { it.copy(volume = clamped) }
        volumeTouchedAt = clock()
        volumes.trySend(out to clamped)
    }

    private suspend fun sendVolume(out: Output.Sonos, level: Int) {
        if (output !== out) return
        try {
            api.command(out.target.id, SonosCommand.Volume(level))
        } catch (e: SonosException) {
            if (output !== out) return
            if (e.sessionGone) {
                endRemote()
                return
            }
            // The next poll shows the volume the group really has.
            volumeTouchedAt = null
            told.tryEmit(e.describe(out.target.name))
        }
    }

    /**
     * Queues a command the UI already shows. If the group can't be reached, the session shows as
     * it was [before], since the speaker never heard it.
     */
    private fun send(out: Output.Sonos, command: SonosCommand, before: SeenSession) {
        commands++
        transport.trySend { deliver(out, command, before) }
    }

    private suspend fun deliver(out: Output.Sonos, command: SonosCommand, before: SeenSession) {
        if (output !== out) return
        try {
            val player = api.command(out.target.id, command)
            if (output === out) follow(out, listOf(player))
        } catch (e: SonosException) {
            if (output !== out) return
            if (e.sessionGone) {
                endRemote()
                return
            }
            setShown(before.playing, before.positionMs)
            told.tryEmit(e.describe(out.target.name))
        }
    }

    // ==========================================================================================
    // Remote mode
    // ==========================================================================================

    private fun enterRemote(target: SonosTarget, episodeId: String, atMs: Long, playing: Boolean) {
        stopPolling()
        val out = Output.Sonos(target, episodeId)
        missingSince = null
        published.update { it.copy(output = out, session = SeenSession(playing, atMs, clock())) }
        polling = scope.launch(serial) {
            while (true) {
                val interval = if (foreground.value) SONOS_POLL_FOREGROUND_MS else SONOS_POLL_BACKGROUND_MS
                // Coming back to the foreground polls at once.
                withTimeoutOrNull(interval) { foreground.drop(1).first { it } }
                pollOnce(out)
            }
        }
    }

    private suspend fun pollOnce(out: Output.Sonos) {
        val sent = commands
        // A failed poll changes nothing; the next one tries again.
        val players = refresh() ?: return
        if (output === out && sent == commands) follow(out, players)
    }

    private fun follow(out: Output.Sonos, players: List<SonosPlayer>) {
        followVolume(out.target, players)
        when (val step = stepSonos(players, out.episodeId, out.target.id, missingSince, clock())) {
            is SonosStep.Show -> {
                missingSince = null
                setShown(step.session.playing, step.session.positionMs)
            }
            is SonosStep.Wait -> missingSince = step.since
            SonosStep.Leave -> endRemote()
        }
    }

    /** Shows the group's volume from a player list, unless the user just moved it. */
    private fun followVolume(target: SonosTarget, players: List<SonosPlayer>) {
        val player = players.find { it.id == target.id } ?: return
        if (adoptsPolledVolume(volumeTouchedAt, clock())) published.update { it.copy(volume = player.volume) }
    }

    /**
     * The session ended on the group's side: the episode finished, or the group plays something
     * else now. The episode stays parked here, and a sync pulls the position and played mark
     * pod-server wrote.
     */
    private fun endRemote() {
        leaveRemote()
        requestSync()
    }

    private fun leaveRemote() {
        stopPolling()
        missingSince = null
        volumeTouchedAt = null
        published.update { it.copy(output = Output.Local, session = null, volume = null) }
    }

    private fun stopPolling() {
        polling?.cancel()
        polling = null
    }

    /** The session as the UI shows it now, with the clock run on since the last poll. */
    private fun shown(): SeenSession {
        val seen = published.value.session ?: return SeenSession(false, 0, clock())
        val now = clock()
        return SeenSession(seen.playing, seen.positionAt(now, durationMs = null), now)
    }

    private fun setShown(playing: Boolean, atMs: Long) {
        published.update { it.copy(session = SeenSession(playing, atMs, clock())) }
    }
}

private const val TAG = "PodSonos"
