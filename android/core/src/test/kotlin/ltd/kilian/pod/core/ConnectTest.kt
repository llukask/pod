package ltd.kilian.pod.core

import kotlinx.serialization.json.Json
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ConnectTest {
    private val episode = "0192f0a0-0000-7000-8000-000000000001"
    private val podcast = "0192f0a0-0000-7000-8000-000000000002"
    private val phone = "0192f0a1-0000-7000-8000-000000000003"
    private val laptop = "0192f0a1-0000-7000-8000-000000000004"
    private val couch = "RINCON_804AF246458401400"

    private fun playback(deviceId: String? = laptop, positionMs: Long = 60_000, playing: Boolean = true, durationMs: Long? = 3_600_000, volume: Int? = null) =
        Playback(deviceId, PlaybackEpisode(episode, podcast, "Episode", "Show", null, durationMs), positionMs, playing, 1.0, volume)

    private fun json(text: String) = Json.parseToJsonElement(text)

    // ==========================================================================================
    // Wire
    // ==========================================================================================

    @Test
    fun eventsParseByType() {
        val state = decodeConnectEvent(
            """{"type": "state", "devices": [{"id": "$phone", "name": "Pixel 8", "kind": "android"}],
                "playback": {"device_id": "$couch", "episode": {"id": "$episode", "podcast_id": "$podcast", "title": "Episode",
                "podcast_title": "Show", "artwork_url": null, "duration_ms": 3600000}, "position_ms": 683000, "playing": true,
                "speed": 1.0, "volume": 10}}""",
        )
        assertEquals(
            ConnectEvent.State(listOf(ConnectDevice(phone, "Pixel 8", DeviceKind.Android)), playback(couch, 683_000, volume = 10)),
            state,
        )
        assertEquals(ConnectEvent.State(emptyList(), null), decodeConnectEvent("""{"type": "state", "devices": [], "playback": null}"""))
        assertEquals(ConnectEvent.Command(ConnectCommand.Skip(-15_000)), decodeConnectEvent("""{"type": "command", "command": {"type": "skip", "delta_ms": -15000}}"""))
        assertEquals(ConnectEvent.Command(ConnectCommand.Pause), decodeConnectEvent("""{"type": "command", "command": {"type": "pause"}}"""))
        assertEquals(ConnectEvent.Load(episode, null), decodeConnectEvent("""{"type": "load", "episode_id": "$episode", "position_ms": null}"""))
        assertEquals(ConnectEvent.Load(episode, 101_500), decodeConnectEvent("""{"type": "load", "episode_id": "$episode", "position_ms": 101500}"""))
        assertEquals(ConnectEvent.Release(null), decodeConnectEvent("""{"type": "release", "transfer_id": null}"""))
        assertEquals(ConnectEvent.Replaced, decodeConnectEvent("""{"type": "replaced"}"""))
    }

    @Test
    fun anEventOfALaterServerIsSkipped() {
        assertNull(decodeConnectEvent("""{"type": "rewind"}"""))
        assertNull(decodeConnectEvent("""{"type": "command", "command": {"type": "shuffle"}}"""))
        assertNull(decodeConnectEvent("not json"))
    }

    @Test
    fun bodiesUseTheSpecNames() {
        fun encode(command: ConnectCommand) = json(WireJson.encodeToString(ConnectCommand.serializer(), command))
        assertEquals(json("""{"type": "pause"}"""), encode(ConnectCommand.Pause))
        assertEquals(json("""{"type": "resume"}"""), encode(ConnectCommand.Resume))
        assertEquals(json("""{"type": "seek", "position_ms": 90000}"""), encode(ConnectCommand.Seek(90_000)))
        assertEquals(json("""{"type": "skip", "delta_ms": -15000}"""), encode(ConnectCommand.Skip(-15_000)))
        assertEquals(json("""{"type": "play", "episode_id": "$episode"}"""), encode(ConnectCommand.Play(episode)))
        assertEquals(json("""{"type": "volume", "volume": 42}"""), encode(ConnectCommand.Volume(42)))
        assertEquals(
            json("""{"episode_id": "$episode", "position_ms": 101500, "playing": false, "speed": 1.5, "released": "t-1"}"""),
            json(WireJson.encodeToString(ConnectReport.serializer(), ConnectReport(episode, 101_500, false, 1.5, "t-1"))),
        )
        assertEquals(json("""{"device_id": "$couch"}"""), json(WireJson.encodeToString(TransferRequest.serializer(), TransferRequest(couch))))
        assertEquals("android", DeviceKind.Android.wire)
    }

    // ==========================================================================================
    // The event stream
    // ==========================================================================================

    @Test
    fun theParserReadsDataLinesAndSkipsComments() {
        val parser = SseParser()
        assertEquals(listOf("""{"type": "replaced"}"""), parser.feed(": ping\n\ndata: {\"type\": \"replaced\"}\n\n"))
        assertEquals(emptyList<String>(), parser.feed(": ping\n\n"))
        assertEquals(listOf("a", "b"), parser.feed("data: a\n\nevent: x\nid: 7\ndata:b\n\n"))
    }

    @Test
    fun anEventSplitAcrossReadsArrivesOnceComplete() {
        val parser = SseParser()
        assertEquals(emptyList<String>(), parser.feed("da"))
        assertEquals(emptyList<String>(), parser.feed("ta: {\"type\":"))
        assertEquals(emptyList<String>(), parser.feed(" \"state\"}\r"))
        assertEquals(emptyList<String>(), parser.feed("\n"))
        assertEquals(listOf("{\"type\": \"state\"}"), parser.feed("\r\n"))
    }

    @Test
    fun aCharacterSplitAcrossReadsDecodesWhole() {
        val parser = SseParser()
        val bytes = "data: Küche\n\n".encodeToByteArray()
        val split = bytes.indexOfFirst { it == 0xC3.toByte() } + 1
        assertEquals(emptyList<String>(), parser.feed(bytes.copyOfRange(0, split)))
        assertEquals(listOf("Küche"), parser.feed(bytes.copyOfRange(split, bytes.size)))
    }

    @Test
    fun dataLinesOfOneEventJoinAndLoneCrEndsALine() {
        assertEquals(listOf("a\nb"), SseParser().feed("data: a\rdata: b\r\r"))
        assertEquals(listOf(""), SseParser().feed("data\n\n"))
    }

    @Test
    fun reconnectsBackOffToThirtySecondsAndStartOverAfterAState() {
        val delay = ReconnectDelay()
        assertEquals(listOf(1_000L, 2_000L, 4_000L, 8_000L, 16_000L, 30_000L, 30_000L), List(7) { delay.next() })
        delay.reset()
        assertEquals(1_000L, delay.next())
    }

    // ==========================================================================================
    // Who plays
    // ==========================================================================================

    @Test
    fun aDeviceIsARemoteExactlyWhenAnotherIsActive() {
        assertEquals(ConnectRole.Active, connectRole(playback(phone), phone))
        assertEquals(ConnectRole.Remote, connectRole(playback(laptop), phone))
        assertEquals(ConnectRole.Remote, connectRole(playback(couch), phone))
        assertEquals(ConnectRole.Idle, connectRole(playback(null), phone))
    }

    @Test
    fun sonosGroupsAreTheIdsThatArentUuids() {
        assertTrue(isSonosDevice(couch))
        assertFalse(isSonosDevice(laptop))
    }

    @Test
    fun aRemoteEndsWhenNothingIsActiveAnyMore() {
        assertTrue(remoteEnded(wasRemote = true, playback(null)))
        assertFalse(remoteEnded(wasRemote = true, playback(laptop)))
        assertFalse(remoteEnded(wasRemote = false, playback(null)))
        assertFalse(remoteEnded(wasRemote = true, null))
    }

    @Test
    fun unaskedReportsComeOnlyFromAPlayingOrActiveDevice() {
        assertTrue(reportsUnasked(playingLocally = true, ConnectRole.Remote))
        assertTrue(reportsUnasked(playingLocally = false, ConnectRole.Active))
        assertFalse(reportsUnasked(playingLocally = false, ConnectRole.Idle))
        assertFalse(reportsUnasked(playingLocally = false, null))
    }

    // ==========================================================================================
    // Showing a remote playback
    // ==========================================================================================

    @Test
    fun thePositionRunsOnBySpeedWhilePlayingUpToTheDuration() {
        assertEquals(61_500, playbackPositionAt(60_000, seenAt = 1_000, playing = true, speed = 1.0, now = 2_500, durationMs = null))
        assertEquals(63_000, playbackPositionAt(60_000, seenAt = 1_000, playing = true, speed = 2.0, now = 2_500, durationMs = null))
        assertEquals(60_000, playbackPositionAt(60_000, seenAt = 1_000, playing = false, speed = 1.0, now = 2_500, durationMs = null))
        assertEquals(61_000, playbackPositionAt(60_000, seenAt = 1_000, playing = true, speed = 1.0, now = 9_000, durationMs = 61_000))
        // Feed durations are approximate (spec 13.3): a position the server reports past one stands.
        assertEquals(65_000, playbackPositionAt(65_000, seenAt = 1_000, playing = true, speed = 1.0, now = 9_000, durationMs = 61_000))
    }

    @Test
    fun aCommandShowsItsEffectAtOnce() {
        val shown = playback(positionMs = 60_000, durationMs = 100_000)
        assertEquals(shown.copy(playing = false, positionMs = 61_000), shown.after(ConnectCommand.Pause, 61_000))
        assertEquals(shown.copy(positionMs = 46_000), shown.after(ConnectCommand.Skip(-15_000), 61_000))
        assertEquals(shown.copy(positionMs = 0), shown.after(ConnectCommand.Skip(-90_000), 61_000))
        assertEquals(shown.copy(positionMs = 100_000), shown.after(ConnectCommand.Skip(90_000), 61_000))
        assertEquals(shown.copy(positionMs = 100_000), shown.after(ConnectCommand.Seek(200_000), 61_000))
        assertEquals(shown.copy(positionMs = 61_000, volume = 30), shown.after(ConnectCommand.Volume(30), 61_000))
        val paused = shown.copy(playing = false)
        assertEquals(paused.copy(playing = true), paused.after(ConnectCommand.Resume, 60_000))
    }

    @Test
    fun theServersVolumeWaitsThreeSecondsAfterTheUserMovedIt() {
        assertTrue(adoptsServerVolume(touchedAt = null, now = 0))
        assertFalse(adoptsServerVolume(touchedAt = 10_000, now = 10_000 + CONNECT_VOLUME_HOLD_MS - 1))
        assertTrue(adoptsServerVolume(touchedAt = 10_000, now = 10_000 + CONNECT_VOLUME_HOLD_MS))
    }

    // ==========================================================================================
    // Events for the local player
    // ==========================================================================================

    @Test
    fun eventsBecomeWhatTheLocalPlayerDoes() {
        assertEquals(PlayerAction.Pause, playerAction(ConnectEvent.Command(ConnectCommand.Pause)))
        assertEquals(PlayerAction.Resume, playerAction(ConnectEvent.Command(ConnectCommand.Resume)))
        assertEquals(PlayerAction.SeekTo(5_000), playerAction(ConnectEvent.Command(ConnectCommand.Seek(5_000))))
        assertEquals(PlayerAction.SkipBy(-15_000), playerAction(ConnectEvent.Command(ConnectCommand.Skip(-15_000))))
        // `play` and a `load` without a position use the start rule, a replay included.
        assertEquals(PlayerAction.Start(episode), playerAction(ConnectEvent.Command(ConnectCommand.Play(episode))))
        assertEquals(PlayerAction.Start(episode), playerAction(ConnectEvent.Load(episode, null)))
        assertEquals(PlayerAction.StartAt(episode, 101_500), playerAction(ConnectEvent.Load(episode, 101_500)))
        assertEquals(PlayerAction.Release("t-1"), playerAction(ConnectEvent.Release("t-1")))
        assertEquals(PlayerAction.Release(null), playerAction(ConnectEvent.Release(null)))
        assertNull(playerAction(ConnectEvent.Command(ConnectCommand.Volume(10))))
        assertNull(playerAction(ConnectEvent.Replaced))
        assertNull(playerAction(ConnectEvent.State(emptyList(), null)))
    }
}
