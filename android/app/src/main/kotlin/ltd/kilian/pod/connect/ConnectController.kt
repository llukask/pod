package ltd.kilian.pod.connect

import android.util.Log
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import ltd.kilian.pod.core.ConnectCommand
import ltd.kilian.pod.core.ConnectDevice
import ltd.kilian.pod.core.ConnectEvent
import ltd.kilian.pod.core.ConnectReport
import ltd.kilian.pod.core.ConnectRole
import ltd.kilian.pod.core.Playback
import ltd.kilian.pod.core.PlayerAction
import ltd.kilian.pod.core.ReconnectDelay
import ltd.kilian.pod.core.SonosPlayer
import ltd.kilian.pod.core.adoptsServerVolume
import ltd.kilian.pod.core.after
import ltd.kilian.pod.core.connectRole
import ltd.kilian.pod.core.isSonosDevice
import ltd.kilian.pod.core.playbackPositionAt
import ltd.kilian.pod.core.playerAction
import ltd.kilian.pod.core.remoteEnded
import ltd.kilian.pod.sync.monotonicMs

// ==============================================================================================
// Connect: one playback across the account's devices (spec 14)
// ==============================================================================================
//
// This controller holds the event stream, keeps the account's playback as the server last told
// it, and is the player while another device is active: it sends the commands of the remote's
// buttons, shows their effect at once and takes it back when the request fails. Transfers start
// here too, from the output picker.
//
// The playback service owns the local player, so the events addressed to it (`command`, `load`,
// `release`) go to the service as [PlayerAction]s, and the service sends the reports (14.3)
// through [report] the way it makes the writes of spec 8.
//
// All state lives on one serial dispatcher. A request suspends there, so another command can run
// meanwhile; each checks afterwards that the device it addressed is still the active one.

/** A device or Sonos group the playback can play on. */
data class Target(val id: String, val name: String, val sonos: Boolean)

/** Where the playback plays, as this device's player shows it. */
sealed interface Output {
    /** Here, or nothing is active and play starts here. */
    data object Local : Output

    /** A transfer to [target] is in flight. One to a Sonos group can take half a minute on a slow podcast host. */
    data class Connecting(val target: Target) : Output

    /** [target] is the active device, and this device is its remote. */
    data class Remote(val target: Target) : Output
}

/** The playback as last heard, at the [monotonicMs] instant [seenAt]. */
data class SeenPlayback(val playback: Playback, val seenAt: Long) {
    /** Where it stands at [now], run on by the elapsed time while it plays (14.1). */
    fun positionAt(now: Long): Long =
        playbackPositionAt(playback.positionMs, seenAt, playback.playing, playback.speed, now, playback.episode.durationMs)
}

data class ConnectView(
    /** This device's id, null while signed out. */
    val selfId: String? = null,
    /** Whether the event stream is open. Reports go out only then (14.3). */
    val streaming: Boolean = false,
    /** The connected client devices, this one included. */
    val devices: List<ConnectDevice> = emptyList(),
    /** The account's playback, null without a stream or while the account never played anything. */
    val seen: SeenPlayback? = null,
    val output: Output = Output.Local,
    /** The Sonos group volume shown, 0 to 100, while a group is active. */
    val volume: Int? = null,
    /** Whether the server listed any Sonos group at the last fetch. */
    val sonosAvailable: Boolean = false,
) {
    /** This device's role in the playback, null without one. */
    val role: ConnectRole? get() = selfId?.let { self -> seen?.playback?.let { connectRole(it, self) } }

    /** The connected devices besides this one, for the output picker. */
    val others: List<ConnectDevice> get() = devices.filter { it.id != selfId }
}

/** A [PlayerAction] for the playback service, and the [monotonicMs] instant its event arrived. */
class Delivered(val action: PlayerAction, val at: Long)

/** An event older than this when the playback service gets to it is dropped, rather than acted on long after it was meant. */
const val ACTION_STALE_MS = 10_000L

class ConnectController(
    private val server: ConnectServer,
    /** The signed-in device's id, null while signed out. */
    private val deviceId: Flow<String?>,
    /** Whether an activity is visible. The stream is held then, or while the playback service exists (14.7). */
    private val foreground: Flow<Boolean>,
    /** Whether the OS reports a network. */
    private val connected: Flow<Boolean>,
    /** Lands the player's queued writes and pushes the outbox, so the server starts a Sonos group from them. */
    private val flushWrites: suspend () -> Unit,
    /** Asks the sync engine for a sync now (spec 5.6). */
    private val requestSync: () -> Unit,
    /** Starts the playback service for an event when it isn't running. An activity is visible then, which allows it. */
    private val startPlayer: () -> Unit,
    private val scope: CoroutineScope,
    private val clock: () -> Long = ::monotonicMs,
) {
    @OptIn(ExperimentalCoroutinesApi::class)
    private val serial = Dispatchers.Default.limitedParallelism(1)

    private val published = MutableStateFlow(ConnectView())
    val view: StateFlow<ConnectView> = published.asStateFlow()

    private val told = MutableSharedFlow<String>(extraBufferCapacity = 8)

    /** Failures to show in a snackbar. */
    val messages: SharedFlow<String> = told.asSharedFlow()

    private val delivered = Channel<Delivered>(Channel.UNLIMITED)

    /** The events for the local player, in order. The playback service takes them while it exists. */
    val actions: ReceiveChannel<Delivered> get() = delivered

    private val serviceAlive = MutableStateFlow(false)

    // The model, read and written on [serial] only. [publish] makes the view of it.
    private var selfId: String? = null
    private var streaming = false
    private var devices = emptyList<ConnectDevice>()
    private var seen: SeenPlayback? = null
    private var connecting: Target? = null
    private var volume: Int? = null
    private var sonosAvailable = false
    private val sonosNames = mutableMapOf<String, String>()

    /** The Sonos groups whose names were asked for once, having been active before the picker listed them. */
    private val namesAsked = mutableSetOf<String>()

    /** Whether this device was a remote at the last playback heard, kept across reconnects for "Remote ended" (14.7). */
    private var wasRemote = false

    /** When the user last moved the volume, or null when the server's may show. */
    private var volumeTouchedAt: Long? = null

    /** Set by a `replaced` event: another instance speaks for this device, so this one stops reconnecting. */
    private var replaced = false
    private val reconnect = ReconnectDelay()

    /** The remote's commands, sent one at a time in the order given, so a quick pause and resume arrive in that order. */
    private val transport = Channel<suspend () -> Unit>(Channel.UNLIMITED)

    /**
     * The volume changes to send. Conflated, so while one request is out only the newest value
     * waits: a dragged slider sends the value it ends on, not a request for every step.
     */
    private val volumes = Channel<Pair<Target, Int>>(Channel.CONFLATED)

    /** The service's reports, in order. Apart from [transport], so a `release` is answered while a transfer waits for it. */
    private val reports = Channel<ConnectReport>(Channel.UNLIMITED)

    init {
        scope.launch(serial) { for (send in transport) send() }
        scope.launch(serial) { for ((target, level) in volumes) sendVolume(target, level) }
        scope.launch(serial) { for (report in reports) sendReport(report) }
        scope.launch(serial) {
            deviceId.collect {
                selfId = it
                publish()
            }
        }
        scope.launch(serial) {
            combine(deviceId, foreground, serviceAlive, connected) { id, visible, alive, online -> id.takeIf { online && (visible || alive) } }
                .distinctUntilChanged()
                .collectLatest { id -> if (id == null) closed() else hold() }
        }
    }

    private fun onSerial(block: suspend () -> Unit) {
        scope.launch(serial) { block() }
    }

    private fun publish() {
        val active = seen?.playback?.deviceId
        val output = connecting?.let { Output.Connecting(it) }
            ?: if (active != null && active != selfId) Output.Remote(targetOf(active)) else Output.Local
        published.value = ConnectView(selfId, streaming, devices, seen, output, volume, sonosAvailable)
    }

    /** The active device as a [Target], named by the device list or the last Sonos player list. */
    private fun targetOf(id: String): Target = if (isSonosDevice(id)) {
        Target(id, sonosNames[id] ?: "a speaker", sonos = true)
    } else {
        Target(id, devices.find { it.id == id }?.name ?: "another device", sonos = false)
    }

    private val activeId get() = seen?.playback?.deviceId

    // ==========================================================================================
    // The event stream (14.2)
    // ==========================================================================================

    /** Holds the stream until it ends for good: reconnects after a delay, but not after a `401` or a `replaced`. */
    private suspend fun hold() {
        replaced = false
        reconnect.reset()
        while (true) {
            val end = try {
                server.events(
                    onOpen = { withContext(serial) { opened() } },
                    onEvent = { withContext(serial) { handle(it) } },
                )
            } finally {
                // Also when the stream is let go, so no report goes out on a stream that isn't held.
                closed()
            }
            if (end == StreamEnd.Unauthorized) {
                // The engine's next request gets the 401 too and shows the device signed out;
                // signing in again changes the device id, which holds the stream again.
                requestSync()
                return
            }
            if (replaced) return
            delay(reconnect.next())
        }
    }

    private fun opened() {
        streaming = true
        publish()
    }

    private fun closed() {
        streaming = false
        devices = emptyList()
        seen = null
        volume = null
        volumeTouchedAt = null
        publish()
    }

    private fun handle(event: ConnectEvent) {
        when (event) {
            is ConnectEvent.State -> {
                reconnect.reset()
                devices = event.devices
                follow(event.playback)
            }
            ConnectEvent.Replaced -> replaced = true
            else -> playerAction(event)?.let { action ->
                delivered.trySend(Delivered(action, clock()))
                if (!serviceAlive.value) startPlayer()
            }
        }
    }

    /** Takes [playback] as the account's playback now: from a `state` event or a request's answer. */
    private fun follow(playback: Playback?) {
        val now = clock()
        if (remoteEnded(wasRemote, playback)) requestSync()
        val self = selfId
        val role = if (playback != null && self != null) connectRole(playback, self) else null
        wasRemote = role == ConnectRole.Remote
        seen = playback?.let { SeenPlayback(it, now) }
        if (playback?.volume == null) {
            volume = null
            volumeTouchedAt = null
        } else if (adoptsServerVolume(volumeTouchedAt, now)) {
            volume = playback.volume
        }
        val active = playback?.deviceId
        if (role == ConnectRole.Remote && active != null && isSonosDevice(active) && active !in sonosNames && namesAsked.add(active)) {
            onSerial { refreshPlayers() }
        }
        publish()
    }

    /** Asks for the state once, after a command found nothing active, so the player shows what is. */
    private suspend fun refreshState() {
        val state = try {
            server.state()
        } catch (e: ConnectException) {
            return
        }
        if (!streaming) return
        devices = state.devices
        follow(state.playback)
    }

    // ==========================================================================================
    // The playback service's side
    // ==========================================================================================

    /** The playback service exists, which holds the stream (14.7) and takes [actions]. */
    fun serviceStarted() {
        serviceAlive.value = true
    }

    fun serviceStopped() {
        serviceAlive.value = false
    }

    /**
     * Sends the local player's state (14.3), while the stream is open. The answer is the
     * playback after it, which tells this device whether it is still the active one.
     */
    fun report(report: ConnectReport) {
        if (published.value.streaming) reports.trySend(report)
    }

    private suspend fun sendReport(report: ConnectReport) {
        val playback = try {
            server.report(report)
        } catch (e: ConnectException) {
            // `409 no_session`: the stream closed meanwhile, and the reconnect reports again.
            if (!e.noSession) Log.i(TAG, "a report failed: ${e.status} ${e.code}")
            return
        }
        if (streaming) follow(playback)
    }

    /** Shows [message] in a snackbar, for a failure the playback service has no place to show. */
    fun tell(message: String) {
        told.tryEmit(message)
    }

    // ==========================================================================================
    // Remote control (14.4)
    // ==========================================================================================

    /** Pauses the active device's playback, or resumes it. */
    fun toggle() = onSerial {
        val shown = seen ?: return@onSerial
        send(if (shown.playback.playing) ConnectCommand.Pause else ConnectCommand.Resume)
    }

    fun resume() = onSerial { send(ConnectCommand.Resume) }

    fun pause() = onSerial { send(ConnectCommand.Pause) }

    fun seek(toMs: Long) = onSerial { send(ConnectCommand.Seek(maxOf(0, toMs))) }

    /** Skips by [deltaMs], the synced `skip_back_ms` negated or `skip_forward_ms`. */
    fun skip(deltaMs: Long) = onSerial { send(ConnectCommand.Skip(deltaMs)) }

    /** Starts [episodeId] on the active device, by the start rule of spec 8 there. */
    fun play(episodeId: String) = onSerial { send(ConnectCommand.Play(episodeId)) }

    /**
     * Queues a command for the active device and shows its effect at once. If the request
     * fails, the playback shows as it was, since the device never heard it.
     */
    private fun send(command: ConnectCommand) {
        val target = (published.value.output as? Output.Remote)?.target ?: return
        val before = seen ?: return
        val now = clock()
        val shown = SeenPlayback(before.playback.after(command, before.positionAt(now)), now)
        seen = shown
        publish()
        transport.trySend { deliver(target, command, before, shown) }
    }

    private suspend fun deliver(target: Target, command: ConnectCommand, before: SeenPlayback, shown: SeenPlayback) {
        if (activeId != target.id) return
        try {
            val answer = server.command(command)
            // A client device shows the change in its next report; until then the answer is the
            // playback from before it, which would undo what the remote shows.
            if (answer.applied || answer.playback.deviceId != target.id) follow(answer.playback)
        } catch (e: ConnectException) {
            if (activeId != target.id) return
            if (seen === shown) {
                seen = before
                publish()
            }
            // `409 no_session`: the device stopped playing meanwhile, which the state shows.
            if (e.noSession) refreshState() else told.tryEmit(e.describe(target.name))
        }
    }

    /** Sets the active Sonos group's volume, 0 to 100. */
    fun setVolume(level: Int) = onSerial {
        val target = (published.value.output as? Output.Remote)?.target?.takeIf { it.sonos } ?: return@onSerial
        val clamped = level.coerceIn(0, 100)
        volume = clamped
        volumeTouchedAt = clock()
        publish()
        volumes.trySend(target to clamped)
    }

    private suspend fun sendVolume(target: Target, level: Int) {
        if (activeId != target.id) return
        try {
            follow(server.command(ConnectCommand.Volume(level)).playback)
        } catch (e: ConnectException) {
            if (activeId != target.id) return
            if (e.noSession) {
                refreshState()
                return
            }
            volumeTouchedAt = null
            volume = seen?.playback?.volume
            publish()
            told.tryEmit(e.describe(target.name))
        }
    }

    // ==========================================================================================
    // Transfer (14.5) and the output picker
    // ==========================================================================================

    /** This device as a picker entry. */
    fun self(): Target? = published.value.selfId?.let { Target(it, "this device", sonos = false) }

    /**
     * Moves the playback to [target] and plays it there. While this device plays, the server
     * releases it like any other source, which the playback service answers.
     */
    fun transfer(target: Target) = onSerial {
        if (connecting != null) return@onSerial
        if (target.id == selfId && published.value.output == Output.Local) return@onSerial
        connecting = target
        publish()
        try {
            if (target.sonos && activeId == null) {
                // With nothing active, the server starts a group from its own episode_state row
                // (14.5), so a position this device hasn't pushed yet goes first.
                try {
                    flushWrites()
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    Log.w(TAG, "pushing the outbox before a transfer failed", e)
                }
            }
            val playback = server.transfer(target.id)
            connecting = null
            follow(playback)
        } catch (e: ConnectException) {
            connecting = null
            publish()
            told.tryEmit(e.describe(if (target.id == selfId) "this device" else target.name))
        }
    }

    /** Every Sonos group, keeping [ConnectView.sonosAvailable] and the groups' names current. Null when the server can't be asked. */
    suspend fun refreshPlayers(): List<SonosPlayer>? {
        val players = try {
            server.players()
        } catch (e: ConnectException) {
            return null
        }
        withContext(serial) {
            sonosAvailable = players.isNotEmpty()
            players.forEach { sonosNames[it.id] = it.name }
            publish()
        }
        return players
    }

    /** Forgets the account's playback after logout. A remote device plays on; the next account can't steer it. */
    fun signedOut() = onSerial {
        connecting = null
        devices = emptyList()
        seen = null
        wasRemote = false
        volume = null
        volumeTouchedAt = null
        sonosAvailable = false
        sonosNames.clear()
        namesAsked.clear()
        publish()
    }
}

private const val TAG = "PodConnect"
