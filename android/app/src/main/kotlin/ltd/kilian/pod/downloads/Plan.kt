package ltd.kilian.pod.downloads

import ltd.kilian.pod.net.NetworkState

// ==============================================================================================
// The reconciler's decision step (spec 7.4)
// ==============================================================================================
//
// A pure function: a snapshot of the download tables plus the conditions outside them (clock,
// network, free space, the playing episode, which transfers are alive) in, a list of [Action]s
// out. `conformance/downloads.json` drives it, and its runner checks that a second pass over the
// result plans nothing. A port of pod-desktop's `reconcile/plan.rs`.

private const val MIB = 1024L * 1024

/** What budget fitting assumes for an episode with no known size. */
const val UNKNOWN_SIZE = 100 * MIB

/** Below this much free space every download pauses (spec 7.2). */
const val MIN_FREE = 500 * MIB
const val MAX_RUNNING = 2
const val DEFAULT_BUDGET = 2 * 1024 * MIB
const val DEFAULT_GRACE_HOURS = 24.0

/** The `device_setting` keys of spec 7.1. Settings › Downloads writes them. */
const val DOWNLOAD_BUDGET_BYTES = "download_budget_bytes"
const val DOWNLOAD_ON_METERED = "download_on_metered"
const val EVICT_GRACE_HOURS = "evict_grace_hours"

/** How often a pass checks again while downloads wait for free space, which can return without any event the reconciler sees. */
private const val SPACE_RECHECK_MS = 60_000L

// ==============================================================================================
// Inputs
// ==============================================================================================

enum class Source(val wire: String) {
    Pin("pin"),
    Auto("auto");

    companion object {
        fun parse(text: String): Source? = entries.firstOrNull { it.wire == text }
    }
}

/** `download.state` (spec 7.2). */
enum class State(val wire: String) {
    Queued("queued"),
    Transferring("transferring"),
    WaitingRetry("waiting_retry"),
    Complete("complete"),
    Failed("failed");

    companion object {
        fun parse(text: String): State? = entries.firstOrNull { it.wire == text }
    }
}

/** Why a `queued` row isn't running, as stored in `download.wait_reason`. */
enum class WaitReason(val wire: String) {
    Connection("connection"),

    /** Held back on a metered network (spec 7.4). */
    Wifi("wifi"),
    Space("space"),
    Budget("budget");

    companion object {
        fun parse(text: String): WaitReason? = entries.firstOrNull { it.wire == text }
    }
}

/** A `podcast_device` row. */
data class PodcastRule(val podcastId: String, val autoKeep: Int, val autoOnMetered: Boolean)

/** An episode with its listening state, as far as wishes and sizes need it. */
data class PlanEpisode(
    val id: String,
    val podcastId: String,
    val publishedAt: Long,
    val audioBytes: Long?,
    val audioUrl: String,
    val played: Boolean,
    val playedAt: Long?,
    val archived: Boolean,
)

/** A `download_wish` row. */
data class Wish(val episodeId: String, val source: Source, val createdAt: Long, val meteredOk: Boolean)

/** The `download` columns the plan reads. */
data class PlanDownload(
    val episodeId: String,
    val state: State,
    val audioUrl: String,
    /** The durable checkpoint, and for a `complete` row the file's size. */
    val bytesDone: Long,
    val nextAttemptAt: Long?,
    val waitReason: WaitReason?,
    val unwantedSince: Long?,
    val runId: String?,
)

/** The device settings of spec 7.1, with their defaults filled in. */
data class DownloadSettings(
    val budget: Long = DEFAULT_BUDGET,
    val onMetered: Boolean = false,
    val graceMs: Long = hoursToMs(DEFAULT_GRACE_HOURS),
)

fun hoursToMs(hours: Double): Long = (hours * 3_600_000.0).toLong()

/**
 * The download tables. [episodes] holds the episodes of podcasts with `auto_keep > 0` and every
 * episode a wish or row names, keyed by id.
 */
data class Snapshot(
    val podcasts: List<PodcastRule> = emptyList(),
    val episodes: Map<String, PlanEpisode> = emptyMap(),
    val wishes: List<Wish> = emptyList(),
    val downloads: List<PlanDownload> = emptyList(),
    val knownSizes: Map<String, Long> = emptyMap(),
    val settings: DownloadSettings = DownloadSettings(),
)

/** Everything outside the tables a pass depends on. */
data class Conditions(
    val now: Long,
    val network: NetworkState,
    val freeBytes: Long,
    val playing: String?,
    /** Episodes whose transfer job is still alive. A `transferring` row missing here was orphaned by a crash or a cancelled job. */
    val live: Set<String>,
)

// ==============================================================================================
// Output
// ==============================================================================================

/**
 * A row as the snapshot saw it. Row actions only apply while the row still has this `run_id`,
 * so a transfer that moved on in between (or a newer run) isn't overwritten; the pass its end
 * triggers looks again.
 */
data class RowRef(val episodeId: String, val runId: String?)

sealed interface Action {
    data class AddWish(val episodeId: String, val source: Source, val createdAt: Long) : Action
    data class RemoveWish(val episodeId: String, val source: Source) : Action

    /** A new `queued` row. */
    data class Insert(val episodeId: String, val audioUrl: String) : Action
    data class SetUnwanted(val row: RowRef, val since: Long?) : Action

    /** Only on `queued` rows. */
    data class SetWait(val row: RowRef, val reason: WaitReason?) : Action

    /** A due `waiting_retry` row, or an orphaned `transferring` one, back to `queued`. */
    data class Requeue(val row: RowRef) : Action

    /** The episode's `audio_url` changed: back to `queued` under no run, with the old run's files deleted. */
    data class Reset(val row: RowRef, val audioUrl: String) : Action

    /** Stops a live transfer and leaves its row `queued` with [reason]. */
    data class Pause(val row: RowRef, val reason: WaitReason) : Action

    /** Deletes the row and its run's files, cancelling a live transfer. */
    data class Remove(val row: RowRef) : Action

    /** Moves a `queued` row to `transferring` under a new run and starts it. */
    data class Start(val row: RowRef) : Action
}

/** What budget fitting left out, for the downloads page. */
data class Fit(
    /** Auto wishes left out, for "skipped: storage budget". */
    val skipped: Set<String> = emptySet(),
    /** The pins alone exceed the budget, so every auto wish is skipped (spec 7.4's warning). */
    val pinsOverBudget: Boolean = false,
)

data class Plan(
    val actions: List<Action>,
    val fit: Fit,
    /** When a pass should run even if nothing else triggers one (epoch ms). */
    val wakeAt: Long?,
)

// ==============================================================================================
// Planning
// ==============================================================================================

/** Newest published first, ties broken by the larger id (spec 6's order). */
private val newestFirst = compareByDescending<PlanEpisode> { it.publishedAt }.thenByDescending { it.id }

fun plan(snapshot: Snapshot, conditions: Conditions): Plan {
    val (podcasts, episodes, wishes, downloads, knownSizes, settings) = snapshot
    val now = conditions.now
    val actions = mutableListOf<Action>()

    // ------------------------------------------------------------------------------------------
    // Wishes
    // ------------------------------------------------------------------------------------------

    // The auto wishes are exactly the newest `auto_keep` unplayed, unarchived episodes of each
    // podcast. Turning `auto_keep` down or off therefore drops the rest, whichever podcast they
    // belong to.
    val autos = mutableSetOf<String>()
    for (rule in podcasts.filter { it.autoKeep > 0 }) {
        episodes.values
            .filter { it.podcastId == rule.podcastId && !it.played && !it.archived }
            .sortedWith(newestFirst)
            .take(rule.autoKeep)
            .mapTo(autos) { it.id }
    }
    val kept = mutableListOf<Wish>()
    for (wish in wishes) {
        val episode = episodes[wish.episodeId]
        val keep = when {
            // Unsubscribing removes the episodes, and with them the wishes.
            episode == null -> false
            wish.source == Source.Auto -> wish.episodeId in autos
            // Finishing an episode releases its pin; pinning an already played episode sticks
            // until it is played through again.
            else -> !(episode.played && episode.playedAt != null && episode.playedAt > wish.createdAt)
        }
        if (keep) kept += wish else actions += Action.RemoveWish(wish.episodeId, wish.source)
    }
    val newWishes = autos
        .filter { id -> wishes.none { it.source == Source.Auto && it.episodeId == id } }
        .sorted()
        .map { Wish(it, Source.Auto, now, meteredOk = false) }
    newWishes.forEach { actions += Action.AddWish(it.episodeId, Source.Auto, now) }
    kept += newWishes

    // ------------------------------------------------------------------------------------------
    // Budget fitting, before anything downloads, so a pass never fetches a file the next one
    // evicts.
    // ------------------------------------------------------------------------------------------

    fun size(episodeId: String): Long = knownSizes[episodeId] ?: episodes[episodeId]?.audioBytes ?: UNKNOWN_SIZE
    fun rowOf(episodeId: String): PlanDownload? = downloads.firstOrNull { it.episodeId == episodeId }

    val pins = kept.filter { it.source == Source.Pin }.sortedWith(compareBy<Wish> { it.createdAt }.thenBy { it.episodeId })
    val pinned = pins.map { it.episodeId }.toSet()
    val autoEpisodes = kept
        .filter { it.source == Source.Auto && it.episodeId !in pinned }
        .mapNotNull { episodes[it.episodeId] }
        .sortedWith(newestFirst)

    val wished = kept.map { it.episodeId }.toSet()
    val playing = conditions.playing
    val pinnedExtra = playing
        ?.takeIf { it !in wished }
        ?.let(::rowOf)
        ?.takeIf { it.state == State.Complete }
        ?.bytesDone ?: 0

    val target = pins.map { it.episodeId }.toMutableList()
    var targetBytes = target.sumOf(::size)
    val pinsOverBudget = targetBytes > settings.budget
    val skipped = sortedSetOf<String>()
    for (episode in autoEpisodes) {
        val bytes = size(episode.id)
        if (targetBytes + bytes + pinnedExtra <= settings.budget) {
            target += episode.id
            targetBytes += bytes
        } else {
            skipped += episode.id
        }
    }
    val inTarget = target.toSet()
    fun protected(id: String) = id in inTarget || playing == id

    // ------------------------------------------------------------------------------------------
    // Rows: bookkeeping, removal, grace eviction, retries and orphans
    // ------------------------------------------------------------------------------------------

    /** A row as it stands after this pass's earlier actions. */
    class Row(var row: RowRef, var state: State, var bytesDone: Long, var waitReason: WaitReason?, val unwantedSince: Long?)

    val keptRows = mutableListOf<Row>()
    var wakeAt: Long? = null
    fun wake(at: Long) {
        wakeAt = wakeAt?.let { minOf(it, at) } ?: at
    }
    fun live(id: String) = id in conditions.live

    for (download in downloads) {
        val id = download.episodeId
        val row = RowRef(id, download.runId)
        // A file counts as wanted while protected; a download that hasn't finished only while
        // targeted, so the playing episode is never downloaded for being played.
        val wanted = if (download.state == State.Complete) protected(id) else id in inTarget
        if (!wanted && download.state != State.Complete) {
            actions += Action.Remove(row)
            continue
        }
        val unwantedSince = if (protected(id)) null else download.unwantedSince ?: now
        if (unwantedSince != download.unwantedSince) actions += Action.SetUnwanted(row, unwantedSince)
        if (unwantedSince != null) {
            val expires = saturatingAdd(unwantedSince, settings.graceMs)
            if (now >= expires) {
                actions += Action.Remove(row)
                continue
            }
            wake(expires)
        }

        val current = Row(row, download.state, download.bytesDone, download.waitReason, unwantedSince)
        // Only a targeted row restarts; a playing file outside the target stays as it is until it
        // is released.
        val episode = episodes[id]?.takeIf { id in inTarget }
        if (episode != null && episode.audioUrl != download.audioUrl) {
            actions += Action.Reset(current.row, episode.audioUrl)
            current.row = current.row.copy(runId = null)
            current.state = State.Queued
            current.bytesDone = 0
            current.waitReason = null
        }
        val due = when (current.state) {
            State.Transferring -> !live(id)
            State.WaitingRetry -> {
                val at = download.nextAttemptAt
                if (at != null && at > now) {
                    wake(at)
                    false
                } else {
                    true
                }
            }
            else -> false
        }
        if (due) {
            actions += Action.Requeue(current.row)
            current.state = State.Queued
            current.waitReason = null
        }
        keptRows += current
    }
    for (id in target) {
        if (rowOf(id) == null) {
            val episode = episodes.getValue(id)
            actions += Action.Insert(episode.id, episode.audioUrl)
            keptRows += Row(RowRef(episode.id, null), State.Queued, 0, null, null)
        }
    }

    // ------------------------------------------------------------------------------------------
    // Transfers
    // ------------------------------------------------------------------------------------------

    fun meteredOk(id: String): Boolean {
        val pin = kept.firstOrNull { it.source == Source.Pin && it.episodeId == id }
        val podcastAllows = episodes[id]?.let { episode -> podcasts.any { it.podcastId == episode.podcastId && it.autoOnMetered } } ?: false
        return !conditions.network.metered || settings.onMetered || pin?.meteredOk == true || (pin == null && podcastAllows)
    }
    val blocked = when {
        !conditions.network.connected -> WaitReason.Connection
        conditions.freeBytes < MIN_FREE -> WaitReason.Space
        else -> null
    }
    if (blocked == WaitReason.Space) wake(now + SPACE_RECHECK_MS)

    // A running transfer that couldn't start now stops. Disk full is the spec's case; a metered
    // network and a lost connection pause too, so nothing downloads over mobile data behind the
    // user's back.
    var running = 0
    for (current in keptRows) {
        if (current.state != State.Transferring) continue
        val pause = blocked ?: if (!meteredOk(current.row.episodeId)) WaitReason.Wifi else null
        if (pause != null) {
            actions += Action.Pause(current.row, pause)
            current.state = State.Queued
            current.waitReason = pause
        } else {
            running += 1
        }
    }

    // Count what is really on disk: finished files, files in their grace period, the playing file
    // and every partial file. A running transfer counts at its full size, so two starts in one
    // pass can't both claim the same room.
    fun remaining(current: Row) = maxOf(0, size(current.row.episodeId) - current.bytesDone)
    var usage = keptRows.sumOf { if (it.state == State.Transferring) it.bytesDone + remaining(it) else it.bytesDone }
    val evictable = keptRows
        .filter { it.state == State.Complete && it.unwantedSince != null }
        .sortedWith(compareBy<Row> { it.unwantedSince }.thenBy { it.row.episodeId })
        .iterator()

    val waits = mutableListOf<Triple<RowRef, WaitReason?, WaitReason?>>()
    val starts = mutableListOf<RowRef>()
    val evictions = mutableListOf<RowRef>()
    var budgetFull = false
    for (id in target) {
        val current = keptRows.firstOrNull { it.row.episodeId == id && it.state == State.Queued } ?: continue
        // A row whose job is still ending (paused, or its outcome not yet seen) waits for it; a
        // second job would share its `.part`.
        if (live(id)) continue
        val reason: WaitReason? = when {
            blocked != null -> blocked
            !meteredOk(id) -> WaitReason.Wifi
            running >= MAX_RUNNING -> null
            budgetFull -> WaitReason.Budget
            else -> {
                val needed = remaining(current)
                while (usage + needed > settings.budget && evictable.hasNext()) {
                    val oldest = evictable.next()
                    usage -= oldest.bytesDone
                    evictions += oldest.row
                }
                if (usage + needed > settings.budget && id !in pinned) {
                    budgetFull = true
                    WaitReason.Budget
                } else {
                    usage += needed
                    running += 1
                    starts += current.row
                    continue
                }
            }
        }
        waits += Triple(current.row, current.waitReason, reason)
    }
    evictions.forEach { actions += Action.Remove(it) }
    waits.filter { (_, before, after) -> before != after }.forEach { (row, _, reason) -> actions += Action.SetWait(row, reason) }
    starts.forEach { actions += Action.Start(it) }

    return Plan(actions, Fit(skipped, pinsOverBudget), wakeAt)
}

private fun saturatingAdd(a: Long, b: Long): Long {
    val sum = a + b
    return if (((a xor sum) and (b xor sum)) < 0) (if (a < 0) Long.MIN_VALUE else Long.MAX_VALUE) else sum
}
