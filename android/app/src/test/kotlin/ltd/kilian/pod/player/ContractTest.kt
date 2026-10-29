package ltd.kilian.pod.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ContractTest {
    private val thresholdMs = 30_000L

    /** 50 minutes, so 2 % (60 s) is above the 30 s threshold. */
    private val longMs = 50 * 60 * 1000L
    private val now = 1_000_000L

    private fun secs(seconds: Double) = now + (seconds * 1000).toLong()
    private fun unplayedAt(positionMs: Long) = Stored(positionMs, played = false)
    private fun position(positionMs: Long, whilePlaying: Boolean) = Write.Position(positionMs, whilePlaying)

    @Test
    fun playbackStartsAtTheStoredPositionWithoutWriting() {
        val (contract, writes) = Contract.start(unplayedAt(754_000), longMs, thresholdMs, now)
        assertEquals(754_000, contract.positionMs)
        assertTrue(contract.isPlaying)
        assertEquals(emptyList<Write>(), writes)
    }

    @Test
    fun replayingAPlayedEpisodeStartsOverAndUnmarksItOnce() {
        val stored = Stored(longMs, played = true)
        assertEquals(0, stored.startPositionMs)
        val (contract, writes) = Contract.start(stored, longMs, thresholdMs, now)
        assertEquals(0, contract.positionMs)
        assertEquals(listOf(Write.Played(false)), writes)
        assertEquals("only the start writes played = false", emptyList<Write>(), contract.progress(1_000, secs(1.0)))
    }

    @Test
    fun aPlayingEpisodeSavesItsPositionEveryFiveSeconds() {
        val (contract, _) = Contract.start(unplayedAt(0), longMs, thresholdMs, now)
        val saves = (1..24).flatMap { tick -> contract.progress(tick * 500L, secs(tick * 0.5)) }
        assertEquals("12 seconds of progress save at 5 s and 10 s", listOf(position(5_000, true), position(10_000, true)), saves)
    }

    @Test
    fun progressWhilePausedSavesNothing() {
        val (contract, _) = Contract.start(unplayedAt(0), longMs, thresholdMs, now)
        assertEquals(listOf(position(0, false)), contract.pause(secs(1.0)))
        assertEquals("already paused", emptyList<Write>(), contract.pause(secs(2.0)))
        assertEquals(emptyList<Write>(), contract.progress(0, secs(30.0)))
        assertFalse(contract.isPlaying)
    }

    @Test
    fun resumingRestartsTheFiveSecondTimer() {
        val (contract, _) = Contract.start(unplayedAt(0), longMs, thresholdMs, now)
        contract.progress(4_000, secs(4.0))
        contract.pause(secs(4.0))
        contract.resume(secs(60.0))
        assertEquals(emptyList<Write>(), contract.progress(8_000, secs(64.0)))
        assertEquals(listOf(position(9_000, true)), contract.progress(9_000, secs(65.0)))
    }

    @Test
    fun pauseSeekAndStopSaveAtOnceAndResetTheTimer() {
        val (contract, _) = Contract.start(unplayedAt(60_000), longMs, thresholdMs, now)
        contract.progress(63_000, secs(3.0))
        assertEquals(listOf(position(93_000, false)), contract.seek(93_000, secs(3.0)))
        assertEquals("the seek saved, so the next periodic save is 5 s after it", emptyList<Write>(), contract.progress(97_000, secs(7.0)))
        assertEquals("a skip back past the start lands on 0", listOf(position(0, false)), contract.seek(-5_000, secs(7.5)))
        assertEquals(listOf(position(0, false)), contract.stop(secs(8.0)))
    }

    @Test
    fun crossingTheThresholdMarksPlayedOnce() {
        val (contract, _) = Contract.start(unplayedAt(longMs - 31_000), longMs, thresholdMs, now)
        assertEquals(emptyList<Write>(), contract.progress(longMs - 30_000, secs(1.0)))
        assertEquals(listOf(Write.Played(true)), contract.progress(longMs - 29_999, secs(2.0)))
        assertEquals(emptyList<Write>(), contract.progress(longMs - 29_000, secs(3.0)))
        assertEquals("the end doesn't mark it again", listOf(position(longMs, false)), contract.ended(secs(4.0)))
    }

    @Test
    fun aShortEpisodeUsesTwoPercentOfItsDuration() {
        // 10 minutes: 2 % is 12 s, below the 30 s threshold.
        val duration = 600_000L
        val (contract, _) = Contract.start(unplayedAt(0), duration, thresholdMs, now)
        assertEquals(listOf(position(580_000, false)), contract.seek(duration - 20_000, now))
        assertEquals("a seek past the threshold marks it too", listOf(Write.Played(true), position(589_000, false)), contract.seek(duration - 11_000, now))
    }

    @Test
    fun thePlayersDurationReplacesTheFeeds() {
        val (contract, _) = Contract.start(unplayedAt(0), null, thresholdMs, now)
        assertEquals("without a duration there's no threshold", emptyList<Write>(), contract.progress(longMs, secs(1.0)))
        contract.setDuration(longMs + 10_000)
        assertEquals(longMs + 10_000, contract.durationMs)
        assertEquals(listOf(Write.Played(true)), contract.progress(longMs, secs(2.0)))
    }

    @Test
    fun reachingTheEndMarksPlayedAndSavesTheEnd() {
        val (unknown, _) = Contract.start(unplayedAt(0), null, thresholdMs, now)
        assertEquals(listOf(Write.Played(true), position(0, false)), unknown.ended(secs(1.0)))

        val (known, _) = Contract.start(unplayedAt(0), longMs, thresholdMs, now)
        known.progress(longMs - 40_000, secs(1.0))
        assertEquals(listOf(Write.Played(true), position(longMs, false)), known.ended(secs(2.0)))
    }
}
