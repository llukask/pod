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
import androidx.media3.common.util.Util
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionError
import androidx.media3.session.SessionResult
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import com.google.common.util.concurrent.SettableFuture
import kotlin.coroutines.cancellation.CancellationException
import kotlin.math.roundToLong
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonPrimitive
import ltd.kilian.pod.Pod
import ltd.kilian.pod.connect.ACTION_STALE_MS
import ltd.kilian.pod.connect.Output
import ltd.kilian.pod.core.CONNECT_REPORT_EVERY_MS
import ltd.kilian.pod.core.ConnectReport
import ltd.kilian.pod.core.ConnectRole
import ltd.kilian.pod.core.PlayerAction
import ltd.kilian.pod.core.Settings
import ltd.kilian.pod.core.reportsUnasked
import ltd.kilian.pod.pod
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
// episode by id, the sleep timer, the skip intervals) are custom session commands. Skips and
// speed changes from any controller go through [SettingsPlayer], which applies the synced
// settings.
//
// Connect (spec 14): the service is this device's local player. It runs the events the server
// addresses to it (`command`, `load`, `release`) as if pressed here, so their writes follow the
// rules above, and it reports the local player's state the way it makes the writes. While
// another device is active, `ConnectController` is the player: the service keeps the episode
// parked and stopped, refuses to start it, and writes nothing. Stopping ExoPlayer (idle, not
// merely paused) takes down the notification and the lock-screen controls, so they don't offer a
// local play. A play from a Bluetooth button resumes the active device instead.
//
// TODO: drive the remote playback from the lock screen and the notification, with a Media3
// `SimpleBasePlayer` over `ConnectController` as the session's player while remote (spec 13.4,
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

    /** The report asked for in this turn of the main loop, so a burst of changes sends one. */
    private var reportDue: Job? = null

    /** When the last report went out ([monotonicMs]); a playing episode reports every [CONNECT_REPORT_EVERY_MS]. */
    private var reportedAt = 0L

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
        // The service holds the event stream while it exists (spec 14.7).
        pod.connect.serviceStarted()

        // The synced settings, applied as they change: a speed set on another device changes
        // the speed here.
        scope.launch {
            pod.database.invalidationTracker.createFlow("setting").collectLatest {
                val speedBefore = settings.speed
                settings = read { PlayerSettings.load(it) }
                if (exo.playbackParameters.speed != settings.speed.toFloat()) exo.setPlaybackSpeed(settings.speed.toFloat())
                publishExtras()
                if (settings.speed != speedBefore) reportUnasked()
            }
        }
        // A parked episode follows the store, so the mini player shows a position or played mark
        // a sync brought in, and a download that completed since becomes the source.
        scope.launch {
            pod.database.invalidationTracker.createFlow("episode_state", "download", emitInitialState = false).collectLatest { refreshParked() }
        }
        scope.launch {
            restoreLastEpisode()
            followPlayback()
        }
        // The events the server addresses to this device, as if pressed here (spec 14.2).
        scope.launch {
            for (delivered in pod.connect.actions) {
                if (monotonicMs() - delivered.at > ACTION_STALE_MS) continue
                perform(delivered.action)
            }
        }
        // The account's playback, once the stream tells it, replaces the restored episode while
        // nothing plays here (spec 14.7, "Offline and cold start").
        scope.launch {
            pod.connect.view.map { it.seen?.playback?.episode?.id to it.role }.distinctUntilChanged().collect { followPlayback() }
        }
        // A stream that (re)connects while this device plays hears from it at once (spec 14.3).
        scope.launch {
            pod.connect.view.map { it.streaming }.distinctUntilChanged().collect { open -> if (open) reportUnasked() }
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
        pod.connect.serviceStopped()
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

    /** Plays [id] from its start position, here or, while another device is active, there (spec 14.7). */
    private suspend fun playEpisode(id: String) {
        when (pod.connect.view.value.output) {
            is Output.Remote -> pod.connect.play(id)
            is Output.Connecting -> {}
            Output.Local -> playHere(id, atMs = null)
        }
    }

    /**
     * Plays [id] here, ending the play in progress first: from its start position by the rule of
     * spec 8 (a replay included), or from [atMs], which isn't a replay. Then reports, which
     * claims the playback (spec 14.1).
     */
    private suspend fun playHere(id: String, atMs: Long?) {
        val loaded = loadOrSync(id) ?: run {
            pod.connect.tell("That episode isn't on this device yet.")
            return
        }
        val episode = if (atMs == null) loaded else loaded.copy(stored = Stored(atMs, played = false))
        endPlay { contract, now -> contract.stop(now) }
        current = episode
        exo.setMediaItem(mediaItem(episode), episode.stored.startPositionMs)
        exo.prepare()
        startPlay(episode)
        exo.play()
        publishExtras()
        report()
    }

    /**
     * Reads [id], syncing once first when it isn't in the store: another device may have started
     * an episode this one hasn't pulled yet.
     */
    private suspend fun loadOrSync(id: String): PlayableEpisode? {
        read { loadEpisode(it, id, downloadsRoot) }?.let { return it }
        try {
            pod.engine.sync()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            android.util.Log.w("PodPlayer", "syncing for a missing episode failed", e)
        }
        return read { loadEpisode(it, id, downloadsRoot) }
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
        if (playingHere && now - reportedAt >= CONNECT_REPORT_EVERY_MS) report()
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
            report()
        }

        override fun onPositionDiscontinuity(oldPosition: Player.PositionInfo, newPosition: Player.PositionInfo, reason: Int) {
            if (reason != Player.DISCONTINUITY_REASON_SEEK || ownSeek) return
            val episode = current ?: return
            val playing = contract
            if (playing != null) {
                issue(episode, playing.seek(newPosition.positionMs, monotonicMs()))
                report()
            } else if (local) {
                // A seek on a parked episode is still the user's: save where it now stands.
                current = episode.copy(stored = episode.stored.copy(positionMs = newPosition.positionMs))
                issue(episode, listOf(Write.Position(newPosition.positionMs, whilePlaying = false)))
                report()
            }
        }

        override fun onPlaybackStateChanged(playbackState: Int) {
            if (playbackState != Player.STATE_ENDED) return
            endPlay { contract, now -> contract.ended(now) }
            if (sleep == Sleep.EndOfEpisode) sleep = null
            exo.pause()
            publishExtras()
            report()
        }

        override fun onPlayerError(error: androidx.media3.common.PlaybackException) {
            android.util.Log.e("PodPlayer", "playback failed", error)
            endPlay { contract, now -> contract.stop(now) }
            report()
        }
    }

    // ==========================================================================================
    // Connect (spec 14)
    // ==========================================================================================

    /** Whether this device plays: no other device is active and no transfer is in flight. */
    private val local get() = pod.connect.view.value.output == Output.Local

    /** Whether another device is active, so this one is its remote. */
    private val remote get() = pod.connect.view.value.output is Output.Remote

    /** Runs an event addressed to this device as if its button had been pressed here (spec 14.2). */
    private suspend fun perform(action: PlayerAction) {
        when (action) {
            // The listener makes the writes and the report of each, as for a press here.
            PlayerAction.Pause -> exo.pause()
            PlayerAction.Resume -> if (current != null) Util.handlePlayButtonAction(exo)
            is PlayerAction.SeekTo -> if (current != null) exo.seekTo(clampToEpisode(action.positionMs))
            is PlayerAction.SkipBy -> if (current != null) exo.seekTo(clampToEpisode(exo.currentPosition + action.deltaMs))
            is PlayerAction.Start -> playHere(action.episodeId, atMs = null)
            is PlayerAction.StartAt -> playHere(action.episodeId, atMs = action.positionMs)
            is PlayerAction.Release -> release(action.transferId)
        }
    }

    private fun clampToEpisode(positionMs: Long): Long {
        val duration = exo.duration.takeIf { it != C.TIME_UNSET } ?: current?.durationMs
        return maxOf(0, if (duration != null) minOf(positionMs, duration) else positionMs)
    }

    /**
     * Another device takes over: pause here with the ordinary pause write, say where this device
     * stopped, for a transfer the position it goes on from, and follow the playback as a remote.
     */
    private fun release(transferId: String?) {
        exo.pause()
        reportDue?.cancel()
        sendReport(released = transferId)
        stopForRemote()
    }

    /** Ends a local play and leaves the episode parked and idle. */
    private fun stopForRemote() {
        endPlay { contract, now -> contract.stop(now) }
        exo.pause()
        exo.stop()
        publishExtras()
    }

    /**
     * Parks the account's playback while nothing plays here and this device isn't the active one:
     * the episode another device plays, or the last playback when nothing is active, at the local
     * start position for it (spec 14.7). An episode this device doesn't have stays out; the
     * player shows the playback's own metadata for it.
     */
    private suspend fun followPlayback() {
        val view = pod.connect.view.value
        val playback = view.seen?.playback ?: return
        val id = playback.episode.id
        if (contract != null || view.role == ConnectRole.Active || current?.id == id) return
        val episode = read { loadEpisode(it, id, downloadsRoot) } ?: return
        if (contract != null || current?.id == id) return
        // An ended episode leaves the player prepared, which would open the new one's stream.
        if (exo.playbackState != Player.STATE_IDLE) exo.stop()
        park(episode)
        pod.playerWrites.remember(id)
    }

    /** Reports the local player's state (spec 14.3), once for everything that changed in this turn of the main loop. */
    private fun report() {
        if (reportDue?.isActive == true) return
        reportDue = scope.launch(Dispatchers.Main) { sendReport(released = null) }
    }

    /** [report], for a change the user didn't make here, which mustn't claim a playback nobody asked for. */
    private fun reportUnasked() {
        if (reportsUnasked(playingHere, pod.connect.view.value.role)) report()
    }

    private val playingHere get() = contract != null && exo.playWhenReady

    private fun sendReport(released: String?) {
        val episode = current ?: return
        reportedAt = monotonicMs()
        pod.connect.report(ConnectReport(episode.id, maxOf(0, exo.currentPosition), playingHere, settings.speed, released))
    }

    // ==========================================================================================
    // Commands
    // ==========================================================================================

    /**
     * The player controllers see. Skips use the synced intervals, and a speed set from any
     * controller writes the synced setting, so it changes on every device (spec 8, "Settings").
     */
    private inner class SettingsPlayer : ForwardingPlayer(exo) {
        // Another device is active, or a transfer is in flight: nothing starts the episode here.
        // Media3 prepares an idle player before playing it, so both are refused. A play or pause
        // from a controller (a Bluetooth button) steers the active device instead (spec 14.7).
        override fun prepare() {
            if (local) super.prepare()
        }

        override fun play() {
            if (local) super.play() else if (remote) pod.connect.resume()
        }

        override fun pause() {
            if (remote) pod.connect.pause()
            super.pause()
        }

        override fun setPlayWhenReady(playWhenReady: Boolean) {
            if (playWhenReady) play() else pause()
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
                else -> return error()
            }
            return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
        }

        private fun error() = Futures.immediateFuture(SessionResult(SessionError.ERROR_BAD_VALUE))

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
