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
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import ltd.kilian.pod.connect.ConnectController
import ltd.kilian.pod.connect.Output
import ltd.kilian.pod.connect.Target
import ltd.kilian.pod.sync.monotonicMs

/**
 * The UI's handle on [PlaybackService]: a `MediaController` with the now-playing state as a
 * [StateFlow] and the commands the screens need. The service owns playback and every write; this
 * only asks. While another device is active, the state follows [connect] and the transport
 * commands go to that device instead (spec 14.7).
 *
 * Create one per activity, in `onCreate`, and [close] it in `onDestroy`. Connecting starts the
 * service, which restores the last episode paused (the cold-start mini player). Commands issued
 * before the connection is up are queued and sent once it is.
 */
class PlayerConnection(context: Context, private val connect: ConnectController) : AutoCloseable {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    /** The local player as the controller reports it. */
    private val current = MutableStateFlow<NowPlaying?>(null)

    /** The synced skip intervals as the service publishes them, also while the player holds nothing. */
    private val skips = MutableStateFlow(Skips.DEFAULT)
    private var controller: MediaController? = null
    private val pending = mutableListOf<(MediaController) -> Unit>()

    /** Ticks while another device plays, so the shown position runs on between its reports. */
    private val clock = MutableStateFlow(monotonicMs())

    /** Null while there is nothing to show. */
    val state: StateFlow<NowPlaying?> = combine(current, connect.view, skips, clock) { local, view, skips, now -> nowPlaying(local, view, skips, now) }
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
                val view = connect.view.value
                if (view.output is Output.Remote && view.seen?.playback?.playing == true) clock.value = monotonicMs()
            }
        }
    }

    private val output get() = connect.view.value.output

    private fun send(command: (MediaController) -> Unit) {
        controller?.let(command) ?: pending.add(command)
    }

    /** Plays [episodeId] from its start position: here, or on the active device (spec 14.7). */
    fun play(episodeId: String) {
        when (output) {
            Output.Local -> send {
                it.sendCustomCommand(SessionCommand(PlaybackService.COMMAND_PLAY_EPISODE, Bundle.EMPTY), Bundle().apply { putString(PlaybackService.ARG_EPISODE_ID, episodeId) })
            }
            is Output.Connecting -> {}
            is Output.Remote -> connect.play(episodeId)
        }
    }

    /**
     * Plays [episodeId] from [positionMs], a chapter's start (spec 17.4): seeks when the player
     * holds it, else starts it there.
     */
    fun playFrom(episodeId: String, positionMs: Long) {
        val shown = state.value
        if (shown != null && shown.episodeId == episodeId && shown.inPlayer) {
            seekTo(positionMs)
            if (!shown.playing) toggle()
            return
        }
        when (output) {
            Output.Local -> send {
                val args = Bundle().apply {
                    putString(PlaybackService.ARG_EPISODE_ID, episodeId)
                    putLong(PlaybackService.ARG_START_MS, positionMs)
                }
                it.sendCustomCommand(SessionCommand(PlaybackService.COMMAND_PLAY_EPISODE, Bundle.EMPTY), args)
            }
            is Output.Connecting -> {}
            // TODO: starts at the episode's own position there; see `PlaybackService.playEpisode`.
            is Output.Remote -> connect.play(episodeId)
        }
    }

    /** Pauses a playing episode, or plays the one shown. */
    fun toggle() {
        when (output) {
            Output.Local -> {
                // The account's last playback of an episode the local player doesn't hold
                // starts here like any other episode.
                val shown = state.value
                if (shown != null && !shown.inPlayer) play(shown.episodeId) else send { if (it.isPlaying) it.pause() else it.play() }
            }
            is Output.Connecting -> {}
            is Output.Remote -> connect.toggle()
        }
    }

    fun seekTo(positionMs: Long) {
        when (output) {
            Output.Local -> if (state.value?.inPlayer != false) send { it.seekTo(positionMs) }
            is Output.Connecting -> {}
            is Output.Remote -> connect.seek(state.value?.durationMs?.let { positionMs.coerceAtMost(it) } ?: positionMs)
        }
    }

    /** Skips by the synced `skip_back_ms`. */
    fun skipBack() = skip(forward = false)

    /** Skips by the synced `skip_forward_ms`. */
    fun skipForward() = skip(forward = true)

    private fun skip(forward: Boolean) {
        when (output) {
            Output.Local -> if (state.value?.inPlayer != false) send { if (forward) it.seekForward() else it.seekBack() }
            is Output.Connecting -> {}
            is Output.Remote -> skips.value.let { connect.skip(if (forward) it.forwardMs else -it.backMs) }
        }
    }

    /** Moves the playback to [target], or to this device (spec 14.5). */
    fun transfer(target: Target) = connect.transfer(target)

    /** Sets the volume of the active Sonos group. */
    fun setVolume(level: Int) = connect.setVolume(level)

    /** Sets the synced `playback_speed`, which changes it on every device, the active one included. */
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
        val extras = player.sessionExtras
        skips.value = Skips(
            extras.getLong(PlaybackService.EXTRA_SKIP_BACK_MS, Skips.DEFAULT.backMs),
            extras.getLong(PlaybackService.EXTRA_SKIP_FORWARD_MS, Skips.DEFAULT.forwardMs),
        )
        val item = player.currentMediaItem
        if (item == null) {
            current.value = null
            return
        }
        val metadata = item.mediaMetadata
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
            skipBackMs = skips.value.backMs,
            skipForwardMs = skips.value.forwardMs,
            sleepAt = extras.getLong(PlaybackService.EXTRA_SLEEP_AT, 0).takeIf { it > 0 },
            sleepAtEndOfEpisode = extras.getBoolean(PlaybackService.EXTRA_SLEEP_END_OF_EPISODE),
        )
    }

    override fun close() {
        scope.cancel()
        MediaController.releaseFuture(future)
    }
}
