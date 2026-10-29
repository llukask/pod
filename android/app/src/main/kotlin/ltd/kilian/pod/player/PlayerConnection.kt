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
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

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
)

/**
 * The UI's handle on [PlaybackService]: a `MediaController` with the now-playing state as a
 * [StateFlow] and the commands the screens need. The service owns playback and every write; this
 * only asks.
 *
 * Create one per activity, in `onCreate`, and [close] it in `onDestroy`. Connecting starts the
 * service, which restores the last episode paused (the cold-start mini player). Commands issued
 * before the connection is up are queued and sent once it is.
 */
class PlayerConnection(context: Context) : AutoCloseable {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val current = MutableStateFlow<NowPlaying?>(null)
    private var controller: MediaController? = null
    private val pending = mutableListOf<(MediaController) -> Unit>()

    /** Null while no episode is in the player. */
    val state: StateFlow<NowPlaying?> = current.asStateFlow()

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
            // Positions aren't pushed, so read them while playing.
            scope.launch {
                while (isActive) {
                    delay(500)
                    if (connected.isPlaying) refresh()
                }
            }
        }, ContextCompat.getMainExecutor(context))
    }

    private fun send(command: (MediaController) -> Unit) {
        controller?.let(command) ?: pending.add(command)
    }

    /** Plays [episodeId] from its start position: the saved position, or the beginning of a played episode. */
    fun play(episodeId: String) = send {
        it.sendCustomCommand(SessionCommand(PlaybackService.COMMAND_PLAY_EPISODE, Bundle.EMPTY), Bundle().apply { putString(PlaybackService.ARG_EPISODE_ID, episodeId) })
    }

    /** Pauses a playing episode, or plays the one in the player. */
    fun toggle() = send { if (it.isPlaying) it.pause() else it.play() }

    fun seekTo(positionMs: Long) = send { it.seekTo(positionMs) }

    /** Skips by the synced `skip_back_ms`. */
    fun skipBack() = send { it.seekBack() }

    /** Skips by the synced `skip_forward_ms`. */
    fun skipForward() = send { it.seekForward() }

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
