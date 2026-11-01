package ltd.kilian.pod.player

import ltd.kilian.pod.connect.ConnectView
import ltd.kilian.pod.connect.Output
import ltd.kilian.pod.connect.SeenPlayback
import ltd.kilian.pod.connect.Target
import ltd.kilian.pod.core.Playback
import ltd.kilian.pod.core.PlaybackEpisode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test

/** What the player shows for the local player and the account's playback (spec 14.7). */
class NowPlayingTest {
    private val episode = "0192f0a0-0000-7000-8000-000000000001"
    private val other = "0192f0a0-0000-7000-8000-000000000005"
    private val phone = "0192f0a1-0000-7000-8000-000000000003"
    private val laptop = "0192f0a1-0000-7000-8000-000000000004"
    private val skips = Skips(10_000, 45_000)

    private fun local(id: String = episode, positionMs: Long = 5_000, playing: Boolean = false) = NowPlaying(
        episodeId = id, podcastId = "p", title = "Local title", podcastTitle = "Local show", artworkUrl = "local.jpg",
        positionMs = positionMs, durationMs = 3_600_000, playing = playing, speed = 1.5f, source = PlaySource.Download,
        skipBackMs = 15_000, skipForwardMs = 30_000, sleepAt = 99, sleepAtEndOfEpisode = false,
    )

    private fun playback(deviceId: String?, playing: Boolean = true, volume: Int? = null) = Playback(
        deviceId, PlaybackEpisode(episode, "p", "Remote title", "Remote show", "remote.jpg", 3_600_000), 60_000, playing, 1.0, volume,
    )

    private fun view(playback: Playback?, output: Output = Output.Local, volume: Int? = null) =
        ConnectView(selfId = phone, streaming = true, seen = playback?.let { SeenPlayback(it, seenAt = 1_000) }, output = output, volume = volume)

    private val onLaptop = Output.Remote(Target(laptop, "Laptop", sonos = false))

    @Test
    fun withoutAStreamOrWhileThisDevicePlaysTheLocalPlayerShows() {
        val mine = local(playing = true)
        assertSame(mine, nowPlaying(mine, ConnectView(selfId = phone), skips, now = 5_000))
        assertSame(mine, nowPlaying(mine, view(playback(phone)), skips, now = 5_000))
        assertNull(nowPlaying(null, ConnectView(selfId = phone), skips, now = 5_000))
    }

    @Test
    fun aRemoteShowsTheActiveDevicesPlaybackRunOn() {
        val shown = nowPlaying(local(), view(playback(laptop), onLaptop), skips, now = 3_500)!!
        assertEquals(62_500, shown.positionMs)
        assertEquals(true, shown.playing)
        assertEquals(onLaptop, shown.output)
        assertEquals(1.0f, shown.speed)
        assertNull("the sleep timer is the local player's", shown.sleepAt)
        // The local episode lends its metadata.
        assertEquals("Local title", shown.title)
    }

    @Test
    fun aRemoteOfAnEpisodeNotHereShowsThePlaybacksMetadata() {
        val shown = nowPlaying(local(id = other), view(playback(laptop), onLaptop), skips, now = 1_000)!!
        assertEquals(episode, shown.episodeId)
        assertEquals("Remote title", shown.title)
        assertEquals("Remote show", shown.podcastTitle)
        assertEquals(10_000, shown.skipBackMs)
        assertFalse(shown.inPlayer)
        assertEquals(nowPlaying(null, view(playback(laptop), onLaptop), skips, now = 1_000), shown)
    }

    @Test
    fun aSonosGroupShowsItsVolume() {
        val couch = Output.Remote(Target("RINCON_1", "Couch", sonos = true))
        val shown = nowPlaying(local(), view(playback("RINCON_1", volume = 12), couch, volume = 14), skips, now = 1_000)!!
        assertEquals(14, shown.volume)
        assertEquals(true, shown.onSonos)
    }

    @Test
    fun withNothingActiveTheLastPlaybackShowsPausedAtTheLocalPosition() {
        val parked = local(positionMs = 5_000)
        assertSame(parked, nowPlaying(parked, view(playback(null, playing = false)), skips, now = 1_000))
        // Not held here: where the server last had it.
        val shown = nowPlaying(local(id = other), view(playback(null, playing = false)), skips, now = 1_000)!!
        assertEquals(episode, shown.episodeId)
        assertEquals(60_000, shown.positionMs)
        assertEquals(false, shown.playing)
        assertFalse(shown.inPlayer)
        // Something playing here wins until its report claims the playback.
        val playingHere = local(id = other, playing = true)
        assertSame(playingHere, nowPlaying(playingHere, view(playback(null, playing = false)), skips, now = 1_000))
    }

    @Test
    fun aTransferOnItsWayShowsPaused() {
        val connecting = Output.Connecting(Target(laptop, "Laptop", sonos = false))
        val shown = nowPlaying(local(playing = true), view(playback(phone), connecting), skips, now = 1_000)!!
        assertEquals(connecting, shown.output)
        assertEquals(false, shown.playing)
    }
}
