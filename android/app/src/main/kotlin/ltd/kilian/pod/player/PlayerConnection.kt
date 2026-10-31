package ltd.kilian.pod.player

import android.content.ComponentName
import android.content.Context
import android.os.Bundle
import androidx.core.content.ContextCompat
import androidx.media3.common.C
import androidx.media3.common.Player
import androidx.media3.session.MediaController
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionToken
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import ltd.kilian.pod.sonos.Output
import ltd.kilian.pod.sonos.SonosController
import ltd.kilian.pod.sonos.SonosState
import ltd.kilian.pod.sonos.SonosTarget
import ltd.kilian.pod.sync.monotonicMs

/** Where the playing audio comes from, for the "Playing from download" / "Streaming" line (wireframe M8). */
enum class PlaySource { Download, Stream }

/** The sleep timer as the UI sets it (wireframe M8). Device-only and ephemeral. */
sealed interface SleepTimer {
    data class Minutes(val minutes: Int) : SleepTimer

    data object EndOfEpisode : SleepTimer
}

/** The player as the UI shows it: the mini player and the now-playing sheet. */
data class NowPlaying(
    val episodeId: String,
    val podcastId: String?,
    val title: String,
    val podcastTitle: String,
    val artworkUrl: String?,
    val positionMs: Long,
    /** Null until the player or the feed knows it. */
    val durationMs: Long?,
    val playing: Boolean,
    val speed: Float,
    val source: PlaySource?,
    /** The synced skip intervals, for the skip buttons' labels. */
    val skipBackMs: Long,
    val skipForwardMs: Long,
    /** Wall-clock epoch ms at which the sleep timer pauses, null without one. */
    val sleepAt: Long?,
    val sleepAtEndOfEpisode: Boolean,
    /** Where it plays: here, or on a Sonos group (spec 13.4). */
    val output: Output = Output.Local,
    /** The group volume, 0 to 100, while a group plays the episode and its volume is known. */
    val volume: Int? = null,
) {
    /** True while a group has the episode, or is being connected to: the local-only controls (speed, sleep timer) step aside. */
    val remote: Boolean get() = output != Output.Local
}

/**
 * The episode as the UI shows it while [sonos] has it: the session's play state and position,
 * run on to [now] between polls, instead of the parked local player's.
 */
fun NowPlaying.on(sonos: SonosState, now: Long): NowPlaying {
    val session = sonos.session ?: return this
    return when (sonos.output) {
        Output.Local -> this
        is Output.Connecting -> copy(output = sonos.output, playing = false, positionMs = session.positionMs, volume = null)
        is Output.Sonos -> copy(
            output = sonos.output,
            playing = session.playing,
            positionMs = session.positionAt(now, durationMs),
            volume = sonos.volume,
        )
    }
}

/**
 * The UI's handle on [PlaybackService]: a `MediaController` with the now-playing state as a
 * [StateFlow] and the commands the screens need. The service owns playback and every write; this
 * only asks. While a Sonos group has the episode, the state follows [sonos] and the transport
 * commands go to it instead (spec 13.4).
 *
 * Create one per activity, in `onCreate`, and [close] it in `onDestroy`. Connecting starts the
 * service, which restores the last episode paused (the cold-start mini player). Commands issued
 * before the connection is up are queued and sent once it is.
 */
class PlayerConnection(context: Context, private val sonos: SonosController) : AutoCloseable {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    /** The local player as the controller reports it. */
    private val current = MutableStateFlow<NowPlaying?>(null)
    private var controller: MediaController? = null
    private val pending = mutableListOf<(MediaController) -> Unit>()

    /** Ticks while a group plays, so the shown position runs on between polls. */
    private val clock = MutableStateFlow(monotonicMs())

    /** Null while no episode is in the player. */
    val state: StateFlow<NowPlaying?> = combine(current, sonos.state, clock) { local, remote, now -> local?.on(remote, now) }
        .stateIn(scope, SharingStarted.Eagerly, null)

    private val future = MediaController.Builder(context, SessionToken(context, ComponentName(context, PlaybackService::class.java)))
        .setListener(object : MediaController.Listener {
            override fun onExtrasChanged(controller: MediaController, extras: Bundle) = refresh()
        })
        .buildAsync()

    init {
        future.addListener({
            val connected = runCatching { future.get() }.getOrNull() ?: return@addListener
            controller = connected
            connected.addListener(object : Player.Listener {
                override fun onEvents(player: Player, events: Player.Events) = refresh()
            })
            pending.forEach { it(connected) }
            pending.clear()
            refresh()
        }, ContextCompat.getMainExecutor(context))
        // Positions aren't pushed, so read them while playing.
        scope.launch {
            while (isActive) {
                delay(500)
                if (controller?.isPlaying == true) refresh()
                if (sonos.state.value.session?.playing == true) clock.value = monotonicMs()
            }
        }
    }

    private val output get() = sonos.state.value.output

    private fun send(command: (MediaController) -> Unit) {
        controller?.let(command) ?: pending.add(command)
    }

    /** Plays [episodeId] from its start position: the saved position, or the beginning of a played episode. */
    fun play(episodeId: String) = send {
        it.sendCustomCommand(SessionCommand(PlaybackService.COMMAND_PLAY_EPISODE, Bundle.EMPTY), Bundle().apply { putString(PlaybackService.ARG_EPISODE_ID, episodeId) })
    }

    /** Pauses a playing episode, or plays the one in the player. */
    fun toggle() = when (output) {
        Output.Local -> send { if (it.isPlaying) it.pause() else it.play() }
        is Output.Connecting -> {}
        is Output.Sonos -> sonos.toggle()
    }

    fun seekTo(positionMs: Long) = when (output) {
        Output.Local -> send { it.seekTo(positionMs) }
        is Output.Connecting -> {}
        is Output.Sonos -> sonos.seek(state.value?.durationMs?.let { positionMs.coerceAtMost(it) } ?: positionMs)
    }

    /** Skips by the synced `skip_back_ms`. */
    fun skipBack() = skip(forward = false)

    /** Skips by the synced `skip_forward_ms`. */
    fun skipForward() = skip(forward = true)

    private fun skip(forward: Boolean) {
        when (output) {
            Output.Local -> send { if (forward) it.seekForward() else it.seekBack() }
            is Output.Connecting -> {}
            is Output.Sonos -> {
                val now = state.value ?: return
                sonos.skip(if (forward) now.skipForwardMs else -now.skipBackMs, now.durationMs)
            }
        }
    }

    /**
     * Plays the episode in the player on [target] (spec 13.4). From this device, the service
     * pauses it and hands it over; from another group, the session moves where it stood.
     */
    fun handOff(target: SonosTarget) {
        when (val out = output) {
            Output.Local -> send {
                it.sendCustomCommand(
                    SessionCommand(PlaybackService.COMMAND_SONOS_PLAY, Bundle.EMPTY),
                    Bundle().apply {
                        putString(PlaybackService.ARG_PLAYER_ID, target.id)
                        putString(PlaybackService.ARG_PLAYER_NAME, target.name)
                    },
                )
            }
            is Output.Connecting -> {}
            is Output.Sonos -> if (out.target.id != target.id) sonos.switchTo(target)
        }
    }

    /** "This device": releases the group's session and goes on here, playing or paused as it was there. */
    fun takeBack() {
        if (output is Output.Sonos) send { it.sendCustomCommand(SessionCommand(PlaybackService.COMMAND_SONOS_TAKE_BACK, Bundle.EMPTY), Bundle.EMPTY) }
    }

    /** Sets the volume of the group that has the episode. */
    fun setVolume(level: Int) = sonos.setVolume(level)

    /** Sets the synced `playback_speed`, which changes it on every device. */
    fun setSpeed(speed: Float) = send { it.setPlaybackSpeed(speed) }

    /** Sets the synced skip interval (a long press on a skip button), which changes it on every device. */
    fun setSkipInterval(forward: Boolean, ms: Long) = send {
        it.sendCustomCommand(
            SessionCommand(PlaybackService.COMMAND_SKIP_INTERVAL, Bundle.EMPTY),
            Bundle().apply {
                putBoolean(PlaybackService.ARG_FORWARD, forward)
                putLong(PlaybackService.ARG_MS, ms)
            },
        )
    }

    /** Sets or, with null, cancels the sleep timer. */
    fun setSleepTimer(timer: SleepTimer?) = send {
        val minutes = when (timer) {
            null -> 0
            SleepTimer.EndOfEpisode -> PlaybackService.SLEEP_END_OF_EPISODE
            is SleepTimer.Minutes -> timer.minutes
        }
        it.sendCustomCommand(SessionCommand(PlaybackService.COMMAND_SLEEP, Bundle.EMPTY), Bundle().apply { putInt(PlaybackService.ARG_MINUTES, minutes) })
    }

    private fun refresh() {
        val player = controller ?: return
        val item = player.currentMediaItem
        if (item == null) {
            current.value = null
            return
        }
        val metadata = item.mediaMetadata
        val extras = player.sessionExtras
        current.value = NowPlaying(
            episodeId = item.mediaId,
            podcastId = metadata.extras?.getString(PlaybackService.EXTRA_PODCAST_ID),
            title = metadata.title?.toString().orEmpty(),
            podcastTitle = metadata.artist?.toString().orEmpty(),
            artworkUrl = metadata.artworkUri?.toString(),
            positionMs = player.currentPosition,
            durationMs = player.duration.takeIf { it != C.TIME_UNSET } ?: metadata.durationMs,
            playing = player.isPlaying,
            speed = player.playbackParameters.speed,
            source = when (extras.getString(PlaybackService.EXTRA_SOURCE)) {
                PlaybackService.SOURCE_DOWNLOAD -> PlaySource.Download
                PlaybackService.SOURCE_STREAM -> PlaySource.Stream
                else -> null
            },
            skipBackMs = extras.getLong(PlaybackService.EXTRA_SKIP_BACK_MS, PlayerSettings().skipBackMs),
            skipForwardMs = extras.getLong(PlaybackService.EXTRA_SKIP_FORWARD_MS, PlayerSettings().skipForwardMs),
            sleepAt = extras.getLong(PlaybackService.EXTRA_SLEEP_AT, 0).takeIf { it > 0 },
            sleepAtEndOfEpisode = extras.getBoolean(PlaybackService.EXTRA_SLEEP_END_OF_EPISODE),
        )
    }

    override fun close() {
        scope.cancel()
        MediaController.releaseFuture(future)
    }
}
