package ltd.kilian.pod.player

import android.os.Bundle
import androidx.annotation.OptIn
import androidx.core.net.toUri
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.ForwardingPlayer
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionResult
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import com.google.common.util.concurrent.SettableFuture
import kotlin.math.roundToLong
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonPrimitive
import ltd.kilian.pod.Pod
import ltd.kilian.pod.core.Settings
import ltd.kilian.pod.core.SonosObservation
import ltd.kilian.pod.core.observeSonos
import ltd.kilian.pod.pod
import ltd.kilian.pod.sonos.Output
import ltd.kilian.pod.sonos.SonosTarget
import ltd.kilian.pod.sync.LocalChange
import ltd.kilian.pod.sync.monotonicMs

// ==============================================================================================
// The playback service (spec 8)
// ==============================================================================================
//
// The service owns playback and every write it causes, so position saves, the played mark and
// the restore on cold start work with no activity alive. The UI is a `MediaController`
// ([PlayerConnection]) like the notification, the lock screen and Bluetooth buttons; it never
// writes listening state itself.
//
// ExoPlayer reports state changes; a one-second tick reports progress. Both feed the pure
// [Contract], whose writes go through [PlayerWrites]. Commands Media3 has no verb for (play an
// episode by id, the sleep timer, the skip intervals, Sonos hand-offs) are custom session
// commands. Skips and speed changes from any controller go through [SettingsPlayer], which
// applies the synced settings.
//
// While a Sonos group has the episode (spec 13.4), pod-server writes its listening state, so the
// service keeps the episode parked and stopped, refuses to start it, and writes nothing.
// `SonosController` is the player meanwhile. Stopping ExoPlayer (idle, not merely paused) takes
// down the notification and the lock-screen controls, so they don't offer a local play.
//
// TODO: drive the remote session from the lock screen and Bluetooth buttons, with a Media3
// `SimpleBasePlayer` over `SonosController` as the session's player while remote (spec 13.4,
// "Media sessions").

/** The `MediaSessionService` that plays episodes. */
@OptIn(UnstableApi::class)
class PlaybackService : MediaSessionService() {
    private lateinit var pod: Pod
    private lateinit var exo: ExoPlayer
    private lateinit var session: MediaSession
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    /** The downloads root `download.path` is relative to. */
    private val downloadsRoot get() = pod.reconciler.root

    private var settings = PlayerSettings()

    /** The episode in the player: playing, paused, or restored and parked. */
    private var current: PlayableEpisode? = null

    /** The play in progress, null while the episode is parked (not started, ended or stopped). */
    private var contract: Contract? = null

    /** Set around the seeks the service makes itself, so they aren't taken for a user's seek. */
    private var ownSeek = false

    /** Device-only and ephemeral (wireframe M8): the [monotonicMs] at which to pause, or the end of the episode. */
    private var sleep: Sleep? = null

    private sealed interface Sleep {
        data class At(val monotonic: Long, val wall: Long) : Sleep

        data object EndOfEpisode : Sleep
    }

    override fun onCreate() {
        super.onCreate()
        pod = applicationContext.pod
        exo = ExoPlayer.Builder(this)
            .setAudioAttributes(
                AudioAttributes.Builder().setUsage(C.USAGE_MEDIA).setContentType(C.AUDIO_CONTENT_TYPE_SPEECH).build(),
                /* handleAudioFocus = */ true,
            )
            .setHandleAudioBecomingNoisy(true)
            .build()
        exo.addListener(Events())
        session = MediaSession.Builder(this, SettingsPlayer()).setCallback(Callback()).build()
        publishExtras()

        // The synced settings, applied as they change: a speed set on another device changes
        // the speed here.
        scope.launch {
            pod.database.invalidationTracker.createFlow("setting").collectLatest {
                settings = read { PlayerSettings.load(it) }
                if (exo.playbackParameters.speed != settings.speed.toFloat()) exo.setPlaybackSpeed(settings.speed.toFloat())
                publishExtras()
            }
        }
        // A parked episode follows the store, so the mini player shows a position or played mark
        // a sync brought in, and a download that completed since becomes the source.
        scope.launch {
            pod.database.invalidationTracker.createFlow("episode_state", "download", emitInitialState = false).collectLatest { refreshParked() }
        }
        scope.launch {
            restoreLastEpisode()
            pickUpRemote()
        }
        // A session that started from here or was picked up stops whatever plays locally, so the
        // service and pod-server never both write the position. A hand-off has already stopped it.
        scope.launch {
            pod.sonos.state.map { it.output == Output.Local }.distinctUntilChanged().collect { local ->
                if (!local) stopForRemote()
            }
        }
        // Back online, a session may be waiting to be picked up, and the picker learns whether
        // the server has groups.
        scope.launch {
            pod.network.state.map { it.connected }.distinctUntilChanged().drop(1).collect { connected ->
                if (connected) pickUpRemote()
            }
        }
        scope.launch {
            while (isActive) {
                delay(1_000)
                tick()
            }
        }
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession = session

    override fun onDestroy() {
        endPlay { contract, now -> contract.stop(now) }
        scope.cancel()
        session.release()
        exo.release()
        super.onDestroy()
    }

    private suspend fun <T> read(block: (androidx.sqlite.db.SupportSQLiteDatabase) -> T): T =
        withContext(Dispatchers.IO) { block(pod.database.openHelper.readableDatabase) }

    // ==========================================================================================
    // Loading episodes
    // ==========================================================================================

    /** Shows the remembered episode paused at its position (spec 8, "Restore on cold start"). */
    private suspend fun restoreLastEpisode() {
        if (current != null) return
        val episode = read { loadLastEpisode(it, downloadsRoot) } ?: return
        if (current != null) return
        park(episode)
    }

    /** Puts [episode] in the player without starting it. Nothing is prepared, so no stream is opened. */
    private fun park(episode: PlayableEpisode) {
        current = episode
        exo.setMediaItem(mediaItem(episode), episode.stored.startPositionMs)
        publishExtras()
    }

    private suspend fun refreshParked() {
        val parked = current ?: return
        if (contract != null) return
        val fresh = read { loadEpisode(it, parked.id, downloadsRoot) } ?: return
        if (contract != null || current?.id != parked.id || fresh == parked) return
        current = fresh
        if (fresh.source != parked.source) {
            exo.setMediaItem(mediaItem(fresh), fresh.stored.startPositionMs)
        } else if (fresh.stored != parked.stored) {
            seekOwn(fresh.stored.startPositionMs)
        }
        publishExtras()
    }

    /** Plays [id] from its start position, ending the play in progress first. On a Sonos group, it plays there. */
    private suspend fun playEpisode(id: String) {
        val episode = read { loadEpisode(it, id, downloadsRoot) } ?: return
        when (val out = pod.sonos.state.value.output) {
            is Output.Connecting -> return
            is Output.Sonos -> {
                current = episode
                exo.setMediaItem(mediaItem(episode), episode.stored.startPositionMs)
                handOff(out.target)
                return
            }
            Output.Local -> {}
        }
        endPlay { contract, now -> contract.stop(now) }
        current = episode
        exo.setMediaItem(mediaItem(episode), episode.stored.startPositionMs)
        exo.prepare()
        startPlay(episode)
        exo.play()
        publishExtras()
    }

    private fun mediaItem(episode: PlayableEpisode): MediaItem = MediaItem.Builder()
        .setMediaId(episode.id)
        .setUri(episode.source.uri)
        .setMediaMetadata(
            MediaMetadata.Builder()
                .setTitle(episode.title)
                .setArtist(episode.podcastTitle)
                .setArtworkUri(episode.artworkUrl?.toUri())
                .setDurationMs(episode.durationMs)
                .setExtras(Bundle().apply { putString(EXTRA_PODCAST_ID, episode.podcastId) })
                .build(),
        )
        .build()

    // ==========================================================================================
    // The contract
    // ==========================================================================================

    private fun startPlay(episode: PlayableEpisode) {
        val (started, writes) = Contract.start(episode.stored, episode.durationMs, settings.playedThresholdMs, monotonicMs())
        contract = started
        if (exo.currentPosition != started.positionMs) seekOwn(started.positionMs)
        pod.playerWrites.remember(episode.id)
        pod.reconciler.setPlaying(episode.id)
        issue(episode, writes)
    }

    /** Ends the play in progress with [end]'s writes and parks the episode where it stopped. */
    private fun endPlay(end: (Contract, Long) -> List<Write>) {
        val episode = current ?: return
        val ending = contract ?: return
        contract = null
        pod.reconciler.setPlaying(null)
        val writes = end(ending, monotonicMs())
        // Every ending saves the position it ended at; the parked episode starts there next.
        val last = writes.lastOrNull() as? Write.Position
        val played = writes.filterIsInstance<Write.Played>().lastOrNull()?.played ?: episode.stored.played
        current = episode.copy(
            durationMs = ending.durationMs ?: episode.durationMs,
            stored = Stored(last?.positionMs ?: ending.positionMs, played),
        )
        issue(episode, writes)
    }

    private fun issue(episode: PlayableEpisode, writes: List<Write>) = pod.playerWrites.write(changesFor(episode, writes))

    private fun tick() {
        val now = monotonicMs()
        val sleeping = sleep
        if (sleeping is Sleep.At && now >= sleeping.monotonic) {
            sleep = null
            exo.pause()
            publishExtras()
        }
        val playing = contract ?: return
        val episode = current ?: return
        if (exo.duration != C.TIME_UNSET && exo.duration != playing.durationMs) playing.setDuration(exo.duration)
        issue(episode, playing.progress(exo.currentPosition, now))
    }

    private fun seekOwn(positionMs: Long) {
        ownSeek = true
        try {
            exo.seekTo(positionMs)
        } finally {
            ownSeek = false
        }
    }

    private inner class Events : Player.Listener {
        override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
            val now = monotonicMs()
            val playing = contract
            when {
                playWhenReady && playing != null -> playing.resume(now)
                // A play from a controller on a parked episode: Media3 prepares the item (or
                // rewinds an ended one) and this starts the contract at the stored position.
                playWhenReady -> current?.let(::startPlay)
                playing != null -> current?.let { issue(it, playing.pause(now)) }
            }
        }

        override fun onPositionDiscontinuity(oldPosition: Player.PositionInfo, newPosition: Player.PositionInfo, reason: Int) {
            if (reason != Player.DISCONTINUITY_REASON_SEEK || ownSeek) return
            val episode = current ?: return
            val playing = contract
            if (playing != null) {
                issue(episode, playing.seek(newPosition.positionMs, monotonicMs()))
            } else if (local) {
                // A seek on a parked episode is still the user's: save where it now stands.
                current = episode.copy(stored = episode.stored.copy(positionMs = newPosition.positionMs))
                issue(episode, listOf(Write.Position(newPosition.positionMs, whilePlaying = false)))
            }
        }

        override fun onPlaybackStateChanged(playbackState: Int) {
            if (playbackState != Player.STATE_ENDED) return
            endPlay { contract, now -> contract.ended(now) }
            if (sleep == Sleep.EndOfEpisode) sleep = null
            exo.pause()
            publishExtras()
        }

        override fun onPlayerError(error: androidx.media3.common.PlaybackException) {
            android.util.Log.e("PodPlayer", "playback failed", error)
            endPlay { contract, now -> contract.stop(now) }
        }
    }

    // ==========================================================================================
    // Sonos groups (spec 13.4)
    // ==========================================================================================

    private val local get() = pod.sonos.state.value.output == Output.Local

    /**
     * Hands the episode in the player to [target]: the local play ends with its ordinary
     * position write, a played episode writes its replay reset, and the group starts where a
     * local play would have (spec 8).
     */
    private fun handOff(target: SonosTarget) {
        endPlay { contract, now -> contract.stop(now) }
        val episode = current ?: return
        val (startMs, writes) = handOffStart(episode.stored)
        issue(episode, writes)
        current = episode.copy(stored = Stored(startMs, played = false))
        stopForRemote()
        if (exo.currentPosition != startMs) seekOwn(startMs)
        pod.playerWrites.remember(episode.id)
        pod.sonos.start(target, episode.id, startMs)
    }

    /** Ends a local play and leaves the episode parked and idle. */
    private fun stopForRemote() {
        endPlay { contract, now -> contract.stop(now) }
        exo.pause()
        exo.stop()
        publishExtras()
    }

    /**
     * Takes the episode back from the group and goes on here at the position it was released
     * at, playing or paused as it was there. That isn't a replay, whatever the played mark.
     */
    private suspend fun takeBack() {
        val released = pod.sonos.release() ?: return
        val episode = current?.takeIf { it.id == released.episodeId }
            ?: read { loadEpisode(it, released.episodeId, downloadsRoot) }
            ?: return
        val resumed = episode.copy(stored = Stored(released.positionMs, played = false))
        current = resumed
        exo.setMediaItem(mediaItem(resumed), released.positionMs)
        if (released.playing) {
            exo.prepare()
            startPlay(resumed)
            exo.play()
        }
        publishExtras()
    }

    /**
     * Follows a session of the parked episode that played on while this process wasn't running
     * (spec 13.4), unless something already plays here. Also tells the picker whether the server
     * has groups.
     */
    private suspend fun pickUpRemote() {
        val players = pod.sonos.refresh() ?: return
        val episode = current ?: return
        if (contract != null || !local) return
        val seen = observeSonos(players, episode.id, null) as? SonosObservation.Session ?: return
        pod.sonos.follow(seen.player, seen.session)
    }

    // ==========================================================================================
    // Commands
    // ==========================================================================================

    /**
     * The player controllers see. Skips use the synced intervals, and a speed set from any
     * controller writes the synced setting, so it changes on every device (spec 8, "Settings").
     */
    private inner class SettingsPlayer : ForwardingPlayer(exo) {
        // A Sonos group has the episode: nothing starts it here. Media3 prepares an idle player
        // before playing it, so both are refused.
        override fun prepare() {
            if (local) super.prepare()
        }

        override fun play() {
            if (local) super.play()
        }

        override fun setPlayWhenReady(playWhenReady: Boolean) {
            if (local || !playWhenReady) super.setPlayWhenReady(playWhenReady)
        }

        override fun getSeekBackIncrement() = settings.skipBackMs

        override fun getSeekForwardIncrement() = settings.skipForwardMs

        override fun seekBack() = seekTo(maxOf(0, currentPosition - settings.skipBackMs))

        override fun seekForward() {
            val target = currentPosition + settings.skipForwardMs
            seekTo(if (duration == C.TIME_UNSET) target else minOf(target, duration))
        }

        override fun setPlaybackSpeed(speed: Float) {
            // Two decimals, so 1.3f is stored as 1.3 and not 1.2999999523.
            val rounded = (speed * 100.0).roundToLong() / 100.0
            if (rounded !in SPEED_RANGE) return
            super.setPlaybackSpeed(rounded.toFloat())
            settings = settings.copy(speed = rounded)
            pod.playerWrites.write(listOf(LocalChange.Setting(Settings.PLAYBACK_SPEED, JsonPrimitive(rounded))))
        }
    }

    private inner class Callback : MediaSession.Callback {
        override fun onConnect(session: MediaSession, controller: MediaSession.ControllerInfo): MediaSession.ConnectionResult {
            val commands = MediaSession.ConnectionResult.DEFAULT_SESSION_COMMANDS.buildUpon()
                .add(SessionCommand(COMMAND_PLAY_EPISODE, Bundle.EMPTY))
                .add(SessionCommand(COMMAND_SLEEP, Bundle.EMPTY))
                .add(SessionCommand(COMMAND_SKIP_INTERVAL, Bundle.EMPTY))
                .add(SessionCommand(COMMAND_SONOS_PLAY, Bundle.EMPTY))
                .add(SessionCommand(COMMAND_SONOS_TAKE_BACK, Bundle.EMPTY))
                .build()
            return MediaSession.ConnectionResult.AcceptedResultBuilder(session).setAvailableSessionCommands(commands).build()
        }

        override fun onCustomCommand(
            session: MediaSession,
            controller: MediaSession.ControllerInfo,
            customCommand: SessionCommand,
            args: Bundle,
        ): ListenableFuture<SessionResult> {
            when (customCommand.customAction) {
                COMMAND_PLAY_EPISODE -> {
                    val id = args.getString(ARG_EPISODE_ID) ?: return error()
                    scope.launch { playEpisode(id) }
                }
                COMMAND_SLEEP -> {
                    val minutes = args.getInt(ARG_MINUTES, 0)
                    sleep = when {
                        minutes == SLEEP_END_OF_EPISODE -> Sleep.EndOfEpisode
                        minutes > 0 -> Sleep.At(monotonicMs() + minutes * 60_000L, System.currentTimeMillis() + minutes * 60_000L)
                        else -> null
                    }
                    publishExtras()
                }
                COMMAND_SKIP_INTERVAL -> {
                    val ms = args.getLong(ARG_MS, -1)
                    if (ms < 0) return error()
                    val key = if (args.getBoolean(ARG_FORWARD)) Settings.SKIP_FORWARD_MS else Settings.SKIP_BACK_MS
                    settings = if (key == Settings.SKIP_FORWARD_MS) settings.copy(skipForwardMs = ms) else settings.copy(skipBackMs = ms)
                    pod.playerWrites.write(listOf(LocalChange.Setting(key, JsonPrimitive(ms))))
                    publishExtras()
                }
                COMMAND_SONOS_PLAY -> {
                    val id = args.getString(ARG_PLAYER_ID) ?: return error()
                    val name = args.getString(ARG_PLAYER_NAME) ?: return error()
                    if (!local) return error()
                    handOff(SonosTarget(id, name))
                }
                COMMAND_SONOS_TAKE_BACK -> scope.launch { takeBack() }
                else -> return error()
            }
            return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
        }

        private fun error() = Futures.immediateFuture(SessionResult(SessionResult.RESULT_ERROR_BAD_VALUE))

        /** A Bluetooth play button with no session alive resumes the remembered episode. */
        override fun onPlaybackResumption(
            mediaSession: MediaSession,
            controller: MediaSession.ControllerInfo,
        ): ListenableFuture<MediaSession.MediaItemsWithStartPosition> {
            val result = SettableFuture.create<MediaSession.MediaItemsWithStartPosition>()
            scope.launch {
                val episode = current ?: read { loadLastEpisode(it, downloadsRoot) }
                if (episode == null) {
                    result.setException(IllegalStateException("no episode to resume"))
                } else {
                    current = episode
                    result.set(MediaSession.MediaItemsWithStartPosition(listOf(mediaItem(episode)), 0, episode.stored.startPositionMs))
                }
            }
            return result
        }
    }

    /** Publishes what controllers can't read from the player itself (see the `EXTRA_` keys). */
    private fun publishExtras() {
        val extras = Bundle()
        when (current?.source) {
            is Source.Download -> extras.putString(EXTRA_SOURCE, SOURCE_DOWNLOAD)
            is Source.Stream -> extras.putString(EXTRA_SOURCE, SOURCE_STREAM)
            null -> {}
        }
        when (val sleeping = sleep) {
            is Sleep.At -> extras.putLong(EXTRA_SLEEP_AT, sleeping.wall)
            Sleep.EndOfEpisode -> extras.putBoolean(EXTRA_SLEEP_END_OF_EPISODE, true)
            null -> {}
        }
        extras.putLong(EXTRA_SKIP_BACK_MS, settings.skipBackMs)
        extras.putLong(EXTRA_SKIP_FORWARD_MS, settings.skipForwardMs)
        session.setSessionExtras(extras)
    }

    companion object {
        const val COMMAND_PLAY_EPISODE = "ltd.kilian.pod.PLAY_EPISODE"
        const val COMMAND_SLEEP = "ltd.kilian.pod.SLEEP"
        const val COMMAND_SKIP_INTERVAL = "ltd.kilian.pod.SKIP_INTERVAL"

        /** Hands the episode to the Sonos group [ARG_PLAYER_ID], named [ARG_PLAYER_NAME], from this device. */
        const val COMMAND_SONOS_PLAY = "ltd.kilian.pod.SONOS_PLAY"

        /** Releases the Sonos session and goes on here. */
        const val COMMAND_SONOS_TAKE_BACK = "ltd.kilian.pod.SONOS_TAKE_BACK"
        const val ARG_PLAYER_ID = "player_id"
        const val ARG_PLAYER_NAME = "player_name"
        const val ARG_EPISODE_ID = "episode_id"

        /** Sleep timer minutes; 0 cancels, [SLEEP_END_OF_EPISODE] pauses at the end of the episode. */
        const val ARG_MINUTES = "minutes"
        const val SLEEP_END_OF_EPISODE = -1
        const val ARG_FORWARD = "forward"
        const val ARG_MS = "ms"

        const val EXTRA_PODCAST_ID = "podcast_id"
        const val EXTRA_SOURCE = "source"
        const val SOURCE_DOWNLOAD = "download"
        const val SOURCE_STREAM = "stream"

        /** Wall-clock epoch ms at which the sleep timer pauses. */
        const val EXTRA_SLEEP_AT = "sleep_at"
        const val EXTRA_SLEEP_END_OF_EPISODE = "sleep_end_of_episode"
        const val EXTRA_SKIP_BACK_MS = "skip_back_ms"
        const val EXTRA_SKIP_FORWARD_MS = "skip_forward_ms"
    }
}
