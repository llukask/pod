package ltd.kilian.pod.sync

// The sync status in the words of spec 5.6, the same on both clients (pod-desktop's
// `shell/sync_status.rs`).

/** The status line. [now] ([monotonicMs]) and [wallNow] are the same moment on the two clocks the engine state uses. */
fun statusLabel(state: EngineState, waiting: Long, now: Long, wallNow: Long): String = when (state) {
    is EngineState.Idle -> when (val at = state.lastSynced) {
        // Before the first loop of this process ends, or after the local store failed one.
        null -> "Not synced yet"
        else -> when (val minutes = maxOf(0, wallNow - at) / 60_000) {
            0L -> "Synced just now"
            in 1..59 -> "Synced $minutes min ago"
            else -> "Synced ${minutes / 60} h ago"
        }
    }
    is EngineState.Running -> state.backfill?.let { "Syncing… Loading episodes for $it" } ?: "Syncing…"
    EngineState.Offline -> if (waiting == 0L) "Offline" else "Offline · $waiting waiting"
    is EngineState.BackingOff -> {
        // Rounded up, so the countdown never reads "0 s" before the retry starts.
        val seconds = maxOf(1, (state.retryAt - now + 999) / 1000)
        val inTime = if (seconds < 60) "$seconds s" else "${(seconds + 59) / 60} min"
        "Can't reach server · retrying in $inTime"
    }
    EngineState.SignedOut -> "Signed out · Sign in"
}

/** The counts line under the status in Settings › Sync, null when both are zero. */
fun countsLabel(waiting: Long, rejected: Long): String? {
    fun changes(count: Long) = if (count == 1L) "1 change" else "$count changes"
    return listOfNotNull(
        if (waiting > 0) "${changes(waiting)} waiting" else null,
        if (rejected > 0) "${changes(rejected)} couldn't be saved" else null,
    ).takeIf { it.isNotEmpty() }?.joinToString(" · ")
}
