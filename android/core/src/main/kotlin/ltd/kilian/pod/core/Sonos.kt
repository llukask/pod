package ltd.kilian.pod.core

// ==============================================================================================
// Following a session on a Sonos group (spec 13.4)
// ==============================================================================================
//
// pod-server plays the episode on the group and writes its listening state (13.3), so a client
// in remote mode only polls the player list and shows what it says. These are the decisions
// around that poll, kept pure so the timing rules can be tested without a clock or a server.
// They mirror the web client's `player/sonos.ts`.

/** How often remote mode polls `GET /v1/sonos/players` while the app is in the foreground (spec 13.4). */
const val SONOS_POLL_FOREGROUND_MS = 2_000L

/** How often it polls otherwise. */
const val SONOS_POLL_BACKGROUND_MS = 15_000L

/**
 * How long the group may be missing from the list before its session counts as over. The server
 * leaves out a group that didn't answer this time, and itself ends a session whose group stays
 * silent this long (spec 13.3).
 */
const val SONOS_MISSING_LIMIT_MS = 2 * 60_000L

/**
 * How long after the user moves the volume the polled volume is ignored. A poll sent before the
 * change landed would otherwise jump the slider back for a moment.
 */
const val SONOS_VOLUME_HOLD_MS = 3_000L

/** What a player list says about the session the player shows. */
sealed interface SonosObservation {
    /** The user's session still plays the episode on [player]. */
    data class Session(val player: SonosPlayer, val session: SonosSessionView) : SonosObservation

    /** The session is over: the episode ended, or the group plays something else. */
    data object Ended : SonosObservation

    /** The group is missing from the list. */
    data object Missing : SonosObservation
}

/**
 * Reads the session for [episodeId] out of [players]: on [playerId], or on any group when that
 * is null (picking up a session after a cold start). A session of another episode counts as
 * ended, since the player has nothing to show for it.
 */
fun observeSonos(players: List<SonosPlayer>, episodeId: String, playerId: String?): SonosObservation {
    val candidates = if (playerId == null) players else players.filter { it.id == playerId }
    if (candidates.isEmpty() && playerId != null) return SonosObservation.Missing
    for (player in candidates) {
        val session = player.session
        if (player.state == SonosPlayerState.Pod && session != null && session.episodeId == episodeId) {
            return SonosObservation.Session(player, session)
        }
    }
    return SonosObservation.Ended
}

/** What one player list does to remote mode. */
sealed interface SonosStep {
    /** Show [session] as it is now. */
    data class Show(val player: SonosPlayer, val session: SonosSessionView) : SonosStep

    /** The group is missing, and has been since [since]; stay in remote mode for now. */
    data class Wait(val since: Long) : SonosStep

    /** Leave remote mode and pull the final state with a sync. */
    data object Leave : SonosStep
}

/**
 * The step for [players], where the group has been missing since [missingSince] (null while it
 * was listed) and the clock reads [now]. A missing group keeps remote mode for
 * [SONOS_MISSING_LIMIT_MS].
 */
fun stepSonos(players: List<SonosPlayer>, episodeId: String, playerId: String, missingSince: Long?, now: Long): SonosStep =
    when (val seen = observeSonos(players, episodeId, playerId)) {
        is SonosObservation.Session -> SonosStep.Show(seen.player, seen.session)
        SonosObservation.Ended -> SonosStep.Leave
        SonosObservation.Missing -> {
            val since = missingSince ?: now
            if (now - since >= SONOS_MISSING_LIMIT_MS) SonosStep.Leave else SonosStep.Wait(since)
        }
    }

/** Whether a polled volume replaces the shown one, when the user last moved it at [touchedAt] (null: never). */
fun adoptsPolledVolume(touchedAt: Long?, now: Long): Boolean = touchedAt == null || now - touchedAt >= SONOS_VOLUME_HOLD_MS

/**
 * Where a session seen at [positionMs] at [seenAt] stands at [now]. The speaker reports whole
 * seconds every few seconds, so the clock runs on between polls while it plays, up to
 * [durationMs] when that is known.
 */
fun sonosPositionAt(positionMs: Long, seenAt: Long, playing: Boolean, now: Long, durationMs: Long?): Long {
    if (!playing) return positionMs
    val running = positionMs + maxOf(0, now - seenAt)
    return if (durationMs != null) minOf(running, maxOf(durationMs, positionMs)) else running
}
