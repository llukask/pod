package ltd.kilian.pod.player

import ltd.kilian.pod.connect.ConnectView
import ltd.kilian.pod.connect.Output
import ltd.kilian.pod.core.ConnectRole
import ltd.kilian.pod.core.Playback

/** Where the playing audio comes from, for the "Playing from download" / "Streaming" line (wireframe M8). */
enum class PlaySource { Download, Stream }

/** The sleep timer as the UI sets it (wireframe M8). Device-only and ephemeral. */
sealed interface SleepTimer {
    data class Minutes(val minutes: Int) : SleepTimer

    data object EndOfEpisode : SleepTimer
}

/** The synced skip intervals, which the skip buttons show and send whatever device plays. */
data class Skips(val backMs: Long, val forwardMs: Long) {
    companion object {
        val DEFAULT = PlayerSettings().let { Skips(it.skipBackMs, it.skipForwardMs) }
    }
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
    /** Where it plays: here, on another device or Sonos group, or a transfer on its way there (spec 14). */
    val output: Output = Output.Local,
    /** The Sonos group volume, 0 to 100, while a group plays it. */
    val volume: Int? = null,
    /**
     * False when the local player doesn't hold the episode: the account's last playback, shown
     * from its own metadata because this device doesn't have the episode (spec 14.7).
     */
    val inPlayer: Boolean = true,
) {
    /** True while another device plays, or a transfer is on its way: the local-only sleep timer steps aside. */
    val remote: Boolean get() = output != Output.Local

    /** True while a Sonos group plays or is being connected to. It plays at 1× only, so the speed steps aside for its volume. */
    val onSonos: Boolean
        get() = when (output) {
            Output.Local -> false
            is Output.Connecting -> output.target.sonos
            is Output.Remote -> output.target.sonos
        }
}

/**
 * What the player shows (spec 14.7, "Mini player"): the local player's episode ([local]) while
 * this device plays or the stream says nothing else, else the account's playback in [view], run on
 * to [now] while another device plays it. The local episode lends its metadata when it is the
 * same one; else the playback's own metadata shows, with the synced [skips].
 */
fun nowPlaying(local: NowPlaying?, view: ConnectView, skips: Skips, now: Long): NowPlaying? {
    val seen = view.seen
    val playback = seen?.playback
    return when (val output = view.output) {
        is Output.Connecting -> (playback?.let { shown(it, local, skips) } ?: local)?.copy(output = output, playing = false, volume = null)
        is Output.Remote -> {
            if (playback == null) return local
            shown(playback, local, skips).copy(
                output = output,
                playing = playback.playing,
                positionMs = seen.positionAt(now),
                speed = playback.speed.toFloat(),
                volume = view.volume,
                sleepAt = null,
                sleepAtEndOfEpisode = false,
            )
        }
        // With nothing active, the last playback shows paused: at the local start position when
        // the player holds it (the service parks it), else where the server last had it.
        Output.Local -> when {
            playback == null || view.role != ConnectRole.Idle -> local
            local != null && (local.episodeId == playback.episode.id || local.playing) -> local
            else -> shown(playback, null, skips)
        }
    }
}

/** [playback]'s episode: [local] when it holds the same one, else from the playback's metadata, paused where the server had it. */
private fun shown(playback: Playback, local: NowPlaying?, skips: Skips): NowPlaying {
    local?.takeIf { it.episodeId == playback.episode.id }?.let { return it }
    val episode = playback.episode
    return NowPlaying(
        episodeId = episode.id,
        podcastId = episode.podcastId,
        title = episode.title,
        podcastTitle = episode.podcastTitle.orEmpty(),
        artworkUrl = episode.artworkUrl,
        positionMs = playback.positionMs,
        durationMs = episode.durationMs,
        playing = false,
        speed = playback.speed.toFloat(),
        source = null,
        skipBackMs = skips.backMs,
        skipForwardMs = skips.forwardMs,
        sleepAt = null,
        sleepAtEndOfEpisode = false,
        inPlayer = false,
    )
}
