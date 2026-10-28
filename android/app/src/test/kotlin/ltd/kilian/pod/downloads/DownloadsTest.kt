package ltd.kilian.pod.downloads

import org.junit.Assert.assertEquals
import org.junit.Test

/** Each download state's wording and way out (M7, M9), the same words as pod-desktop's D4. */
class DownloadsTest {
    private val now = 1_000_000_000L
    private val episode = "e0000000-0000-5000-8000-000000000001"

    private fun entry(row: Row?) = Entry(episode, "a1000000-0000-5000-8000-000000000001", "The chip wars", "Hard Fork", pinned = false, auto = true, row = row, size = null)

    private fun shown(entry: Entry, fit: Fit = Fit(), progress: Map<String, Progress> = emptyMap()): Pair<String, List<String>> {
        val status = status(entry, fit, progress)
        return statusLabel(status, now) to actions(status).map { it.label }
    }

    @Test
    fun everyStateHasItsLabelAndAWayOut() {
        val cancel = listOf("Cancel")
        val cases = listOf(
            entry(null) to ("Queued" to cancel),
            entry(Row(State.Queued)) to ("Queued" to cancel),
            entry(Row(State.Queued, WaitReason.Connection)) to ("Waiting · no connection" to cancel),
            entry(Row(State.Queued, WaitReason.Wifi)) to ("Waiting · metered network" to listOf("Download on mobile data anyway", "Cancel")),
            entry(Row(State.Queued, WaitReason.Space)) to ("Paused · storage full" to cancel),
            entry(Row(State.Queued, WaitReason.Budget)) to ("Waiting · doesn't fit the budget" to listOf("Download anyway", "Cancel")),
            entry(Row(State.Transferring)) to ("Downloading" to cancel),
            entry(Row(State.Transferring, bytesDone = 31_000_000, bytesTotal = 50_000_000)) to ("Downloading · 31.0 MB of 50.0 MB" to cancel),
            entry(Row(State.WaitingRetry, attempts = 2, nextAttemptAt = now + 90_000, lastError = "connection reset")) to
                ("Retrying in 2 min · connection reset (attempt 3 of 8)" to cancel),
            entry(Row(State.WaitingRetry, attempts = 1, nextAttemptAt = now - 1)) to ("Retrying now (attempt 2 of 8)" to cancel),
            entry(Row(State.Failed, lastError = "HTTP 404")) to ("Failed · HTTP 404" to listOf("Retry", "Remove")),
            entry(Row(State.Failed)) to ("Failed" to listOf("Retry", "Remove")),
            entry(Row(State.Complete)) to ("Downloaded" to listOf("Remove")),
            entry(Row(State.Complete, leavingAt = now + 20 * 3_600_000)) to ("Downloaded · removed in 20 h" to listOf("Keep", "Remove")),
            entry(Row(State.Complete, leavingAt = now - 1)) to ("Downloaded · removing" to listOf("Keep", "Remove")),
        )
        for ((entry, want) in cases) assertEquals("${entry.row}", want, shown(entry))
    }

    @Test
    fun aWishTheBudgetLeftOutReadsSkipped() {
        assertEquals("Skipped · doesn't fit the budget" to listOf("Download anyway", "Archive"), shown(entry(null), Fit(setOf(episode))))
    }

    @Test
    fun liveProgressOverridesTheCheckpoint() {
        val progress = mapOf(episode to Progress(40_000_000, 50_000_000))
        assertEquals("Downloading · 40.0 MB of 50.0 MB", shown(entry(Row(State.Transferring, bytesDone = 31_000_000)), progress = progress).first)
    }

    @Test
    fun bytesReadInDecimalUnits() {
        assertEquals("999 bytes", formatBytes(999))
        assertEquals("48.0 MB", formatBytes(48_000_000))
        assertEquals("1.0 GB", formatBytes(999_960_000))
        assertEquals("2.1 GB", formatBytes(2_147_483_648))
    }
}
