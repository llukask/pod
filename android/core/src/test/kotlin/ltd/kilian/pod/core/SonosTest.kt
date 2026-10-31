package ltd.kilian.pod.core

import kotlinx.serialization.json.Json
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class SonosTest {
    private val episode = "0192f0a0-0000-7000-8000-000000000001"
    private val other = "0192f0a0-0000-7000-8000-000000000002"
    private val couch = "RINCON_804AF246458401400"

    private fun player(id: String = couch, state: SonosPlayerState = SonosPlayerState.Pod, session: SonosSessionView? = SonosSessionView(episode, true, 683_000)) =
        SonosPlayer(id, "Couch", 10, state, session.takeIf { state == SonosPlayerState.Pod })

    // ==========================================================================================
    // Wire
    // ==========================================================================================

    @Test
    fun playerListsParseWithAndWithoutASession() {
        val body = """
            {"players": [
              {"id": "RINCON_804AF246458401400", "name": "Couch", "volume": 10, "state": "pod",
               "session": {"episode_id": "$episode", "playing": true, "position_ms": 683000}},
              {"id": "RINCON_1", "name": "Couch + Küche", "volume": 35, "state": "other", "session": null},
              {"id": "RINCON_2", "name": "Bad", "volume": 0, "state": "idle"}
            ]}
        """
        val players = WireJson.decodeFromString(SonosPlayersResponse.serializer(), body).players
        assertEquals(SonosSessionView(episode, true, 683_000), players[0].session)
        assertEquals(listOf(SonosPlayerState.Pod, SonosPlayerState.Other, SonosPlayerState.Idle), players.map { it.state })
        assertEquals(listOf(null, null), players.drop(1).map { it.session })
        assertEquals("Couch + Küche", players[1].name)
    }

    @Test
    fun commandsAreTaggedByType() {
        fun encode(command: SonosCommand) = Json.parseToJsonElement(WireJson.encodeToString(SonosCommand.serializer(), command))
        assertEquals(Json.parseToJsonElement("""{"type": "pause"}"""), encode(SonosCommand.Pause))
        assertEquals(Json.parseToJsonElement("""{"type": "resume"}"""), encode(SonosCommand.Resume))
        assertEquals(Json.parseToJsonElement("""{"type": "seek", "position_ms": 90000}"""), encode(SonosCommand.Seek(90_000)))
        assertEquals(Json.parseToJsonElement("""{"type": "skip", "delta_ms": -15000}"""), encode(SonosCommand.Skip(-15_000)))
        assertEquals(Json.parseToJsonElement("""{"type": "volume", "volume": 42}"""), encode(SonosCommand.Volume(42)))
    }

    @Test
    fun releasedAndPlayBodiesUseTheSpecNames() {
        val released = WireJson.decodeFromString(SonosReleased.serializer(), """{"episode_id": "$episode", "position_ms": 700000, "playing": false}""")
        assertEquals(SonosReleased(episode, 700_000, false), released)
        assertEquals(
            Json.parseToJsonElement("""{"episode_id": "$episode", "position_ms": 0}"""),
            Json.parseToJsonElement(WireJson.encodeToString(SonosPlayRequest.serializer(), SonosPlayRequest(episode, 0))),
        )
    }

    // ==========================================================================================
    // Observing a player list
    // ==========================================================================================

    @Test
    fun theUsersSessionOfTheEpisodeIsShown() {
        val seen = observeSonos(listOf(player()), episode, couch)
        assertEquals(SonosObservation.Session(player(), SonosSessionView(episode, true, 683_000)), seen)
    }

    @Test
    fun aSessionOfAnotherEpisodeOrAGroupPlayingSomethingElseHasEnded() {
        assertEquals(SonosObservation.Ended, observeSonos(listOf(player(session = SonosSessionView(other, true, 0))), episode, couch))
        assertEquals(SonosObservation.Ended, observeSonos(listOf(player(state = SonosPlayerState.Other)), episode, couch))
        assertEquals(SonosObservation.Ended, observeSonos(listOf(player(state = SonosPlayerState.Idle)), episode, couch))
    }

    @Test
    fun aGroupMissingFromTheListIsMissingNotEnded() {
        assertEquals(SonosObservation.Missing, observeSonos(listOf(player(id = "RINCON_2")), episode, couch))
        assertEquals(SonosObservation.Missing, observeSonos(emptyList(), episode, couch))
    }

    @Test
    fun withoutAGroupAnyGroupWithTheSessionIsPickedUp() {
        val players = listOf(player(id = "RINCON_2", state = SonosPlayerState.Other), player())
        assertEquals(couch, (observeSonos(players, episode, null) as SonosObservation.Session).player.id)
        assertEquals(SonosObservation.Ended, observeSonos(emptyList(), episode, null))
    }

    // ==========================================================================================
    // Stepping remote mode
    // ==========================================================================================

    @Test
    fun aListedSessionIsShownAndAnEndedOneLeaves() {
        assertEquals(SonosStep.Show(player(), player().session!!), stepSonos(listOf(player()), episode, couch, missingSince = 5_000, now = 10_000))
        assertEquals(SonosStep.Leave, stepSonos(listOf(player(state = SonosPlayerState.Other)), episode, couch, null, 10_000))
    }

    @Test
    fun aMissingGroupKeepsRemoteModeForTwoMinutes() {
        assertEquals(SonosStep.Wait(10_000), stepSonos(emptyList(), episode, couch, missingSince = null, now = 10_000))
        assertEquals(SonosStep.Wait(10_000), stepSonos(emptyList(), episode, couch, missingSince = 10_000, now = 10_000 + SONOS_MISSING_LIMIT_MS - 1))
        assertEquals(SonosStep.Leave, stepSonos(emptyList(), episode, couch, missingSince = 10_000, now = 10_000 + SONOS_MISSING_LIMIT_MS))
    }

    @Test
    fun aPolledVolumeWaitsThreeSecondsAfterTheUserMovedIt() {
        assertTrue(adoptsPolledVolume(touchedAt = null, now = 0))
        assertFalse(adoptsPolledVolume(touchedAt = 10_000, now = 10_000 + SONOS_VOLUME_HOLD_MS - 1))
        assertTrue(adoptsPolledVolume(touchedAt = 10_000, now = 10_000 + SONOS_VOLUME_HOLD_MS))
    }

    @Test
    fun thePositionRunsOnBetweenPollsWhilePlayingUpToTheDuration() {
        assertEquals(61_500, sonosPositionAt(60_000, seenAt = 1_000, playing = true, now = 2_500, durationMs = null))
        assertEquals(60_000, sonosPositionAt(60_000, seenAt = 1_000, playing = false, now = 2_500, durationMs = null))
        assertEquals(61_000, sonosPositionAt(60_000, seenAt = 1_000, playing = true, now = 9_000, durationMs = 61_000))
        // Feed durations are approximate (spec 13.3): a position the speaker reports past one stands.
        assertEquals(65_000, sonosPositionAt(65_000, seenAt = 1_000, playing = true, now = 9_000, durationMs = 61_000))
    }
}
