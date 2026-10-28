package ltd.kilian.pod.sync

import org.junit.Assert.assertEquals
import org.junit.Test

class StatusTest {
    @Test
    fun eachEngineStateHasItsWording() {
        val now = 1_000_000L
        val wallNow = 1_800_000_000_000L
        fun minutesAgo(minutes: Long) = wallNow - minutes * 60_000 - 5_000
        fun label(state: EngineState, waiting: Long) = statusLabel(state, waiting, now, wallNow)
        fun backingOff(delayMs: Long) = EngineState.BackingOff(now + delayMs)

        assertEquals("Synced just now", label(EngineState.Idle(minutesAgo(0)), 0))
        assertEquals("Synced 2 min ago", label(EngineState.Idle(minutesAgo(2)), 0))
        assertEquals("Synced 59 min ago", label(EngineState.Idle(minutesAgo(59)), 0))
        assertEquals("Synced 3 h ago", label(EngineState.Idle(minutesAgo(185)), 0))
        assertEquals("Not synced yet", label(EngineState.Idle(null), 0))
        assertEquals("Syncing…", label(EngineState.Running(null), 2))
        assertEquals("Syncing… Loading episodes for Syntax", label(EngineState.Running("Syntax"), 0))
        assertEquals("Offline · 3 waiting", label(EngineState.Offline, 3))
        assertEquals("Offline", label(EngineState.Offline, 0))
        assertEquals("Can't reach server · retrying in 4 min", label(backingOff(4 * 60_000), 1))
        assertEquals("Can't reach server · retrying in 3 min", label(backingOff(150_000), 1))
        assertEquals("Can't reach server · retrying in 5 s", label(backingOff(4_200), 1))
        assertEquals("Can't reach server · retrying in 1 s", label(backingOff(0), 1))
        assertEquals("Signed out · Sign in", label(EngineState.SignedOut, 4))
    }

    @Test
    fun theCountsLineNamesWhatIsQueuedAndRefused() {
        assertEquals(null, countsLabel(0, 0))
        assertEquals("2 changes waiting", countsLabel(2, 0))
        assertEquals("1 change waiting · 1 change couldn't be saved", countsLabel(1, 1))
        assertEquals("3 changes couldn't be saved", countsLabel(0, 3))
    }
}
