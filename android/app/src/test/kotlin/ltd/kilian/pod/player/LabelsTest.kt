package ltd.kilian.pod.player

import org.junit.Assert.assertEquals
import org.junit.Test

class LabelsTest {
    @Test
    fun clockShowsMinutesAndSecondsUnderAnHour() {
        assertEquals("0:00", clockLabel(0))
        assertEquals("31:12", clockLabel((31 * 60 + 12) * 1000L + 400))
        assertEquals("1:02:03", clockLabel((3600 + 2 * 60 + 3) * 1000L))
    }

    @Test
    fun remainingRoundsUpSoItReachesZeroAtTheEnd() {
        assertEquals("−20:48", remainingLabel((31 * 60 + 12) * 1000L, (52 * 60) * 1000L))
        assertEquals("−0:01", remainingLabel(59_100, 60_000))
        assertEquals("−0:00", remainingLabel(60_000, 60_000))
    }

    @Test
    fun speedDropsTrailingZeros() {
        assertEquals("1×", speedLabel(1.0f))
        assertEquals("1.3×", speedLabel(1.3f))
        assertEquals("1.75×", speedLabel(1.75f))
    }

    @Test
    fun skipIntervalsInSeconds() {
        assertEquals("15 s", skipLabel(15_000))
        assertEquals("1 min", skipLabel(60_000))
    }

    @Test
    fun sleepCountdownRoundsUp() {
        assertEquals("24:00", sleepLabel(sleepAt = 1_000_000 + 24 * 60_000L, sleepAtEndOfEpisode = false, now = 1_000_000))
        assertEquals("0:01", sleepLabel(sleepAt = 1_000_500, sleepAtEndOfEpisode = false, now = 1_000_000))
        assertEquals("End of episode", sleepLabel(sleepAt = null, sleepAtEndOfEpisode = true, now = 0))
        assertEquals(null, sleepLabel(sleepAt = null, sleepAtEndOfEpisode = false, now = 0))
    }
}
