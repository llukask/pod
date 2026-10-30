package ltd.kilian.pod.ui

import java.time.LocalDateTime
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Test

class InboxGroupsTest {
    private val zone = ZoneId.of("Europe/Berlin")
    private fun at(day: Int, hour: Int) = LocalDateTime.of(2026, 10, day, hour, 0).atZone(zone).toInstant().toEpochMilli()
    private val now = at(30, 9)

    @Test
    fun `calendar days pick the group`() {
        assertEquals("Today", dayGroup(at(30, 1), now, zone))
        assertEquals("Yesterday", dayGroup(at(29, 23), now, zone))
        assertEquals("This week", dayGroup(at(28, 12), now, zone))
        assertEquals("This week", dayGroup(at(24, 0), now, zone))
        assertEquals("Earlier", dayGroup(at(23, 23), now, zone))
    }

    @Test
    fun `an episode dated ahead of the clock is today's`() {
        assertEquals("Today", dayGroup(at(31, 8), now, zone))
    }

    @Test
    fun `runs of one group stay together in order`() {
        assertEquals(
            listOf("a" to listOf(1, 2), "b" to listOf(3), "a" to listOf(4)),
            groupByDay(listOf(1, 2, 3, 4)) { if (it == 3) "b" else "a" },
        )
    }
}
