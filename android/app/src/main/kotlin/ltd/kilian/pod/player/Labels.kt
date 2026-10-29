package ltd.kilian.pod.player

// The now-playing sheet's wording (wireframe M8).

/** "31:12", or "1:02:03" from an hour on. Truncates, like every player's elapsed time. */
fun clockLabel(ms: Long): String = clock(ms / 1_000)

/** "−20:48": the time left, rounded up so it reads "−0:00" only at the end. */
fun remainingLabel(positionMs: Long, durationMs: Long): String = "−" + clock(ceilSeconds(durationMs - positionMs))

/** "1.3×", the way the speed steps are written. */
fun speedLabel(speed: Float): String = "%.2f".format(java.util.Locale.ROOT, speed).trimEnd('0').trimEnd('.') + "×"

/** "15 s", or whole minutes. */
fun skipLabel(ms: Long): String = if (ms >= 60_000 && ms % 60_000 == 0L) "${ms / 60_000} min" else "${ms / 1_000} s"

/** The sleep timer's countdown, rounded up like [remainingLabel], "End of episode", or null without a timer. */
fun sleepLabel(sleepAt: Long?, sleepAtEndOfEpisode: Boolean, now: Long): String? = when {
    sleepAt != null -> clock(ceilSeconds(sleepAt - now))
    sleepAtEndOfEpisode -> "End of episode"
    else -> null
}

private fun ceilSeconds(ms: Long): Long = (maxOf(ms, 0) + 999) / 1_000

private fun clock(totalSeconds: Long): String {
    val hours = totalSeconds / 3_600
    val minutes = totalSeconds % 3_600 / 60
    val seconds = totalSeconds % 60
    return if (hours > 0) "%d:%02d:%02d".format(hours, minutes, seconds) else "%d:%02d".format(minutes, seconds)
}
