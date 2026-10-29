package ltd.kilian.pod.player

import android.util.Log
import androidx.sqlite.db.SupportSQLiteDatabase
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonPrimitive
import ltd.kilian.pod.core.WireJson
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec
import ltd.kilian.pod.store.flag
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.longOrNull
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull
import ltd.kilian.pod.sync.EpisodeChange
import ltd.kilian.pod.sync.LAST_EPISODE_ID
import ltd.kilian.pod.sync.LocalChange
import ltd.kilian.pod.sync.SyncHandle
import ltd.kilian.pod.sync.writeLocal

// ==============================================================================================
// What the player reads from and writes to the store (spec 8)
// ==============================================================================================

/**
 * Where the audio comes from: the downloaded file when its `download` row is `complete` and the
 * file exists, else the stream. Playing never starts a download.
 */
sealed interface Source {
    val uri: String

    data class Download(val file: File) : Source {
        override val uri get() = file.toURI().toString()
    }

    data class Stream(override val uri: String) : Source

    companion object {
        fun choose(downloaded: File?, audioUrl: String): Source =
            if (downloaded != null && downloaded.isFile) Download(downloaded) else Stream(audioUrl)
    }
}

/**
 * An episode as the player holds it. The player keeps this metadata for the whole play, so an
 * episode a sync deletes mid-play still plays to its end and its writes still name its podcast
 * (spec 8, "Episode removed while playing").
 */
data class PlayableEpisode(
    val id: String,
    val podcastId: String,
    val title: String,
    val podcastTitle: String,
    /** The episode's artwork, else the show's. */
    val artworkUrl: String?,
    /** The feed's duration, until the player reports the file's. */
    val durationMs: Long?,
    val source: Source,
    val stored: Stored,
)

/**
 * Reads [id] for the player, with the source resolved against the downloads [root]. Null when the
 * episode isn't in the store. Checks the file on disk, so it runs off the main thread.
 */
fun loadEpisode(db: SupportSQLiteDatabase, id: String, root: File): PlayableEpisode? =
    db.queryOne(
        """select e.podcast_id, e.title, coalesce(p.title, p.local_title, p.feed_url, '') as podcast_title,
                  e.duration_ms, e.audio_url, coalesce(s.position_ms, 0) as position_ms,
                  coalesce(s.played, 0) as played, d.path,
                  coalesce(e.artwork_url, p.artwork_url, p.local_artwork_url) as artwork
           from episode e
           left join podcast p on p.id = e.podcast_id
           left join episode_state s on s.episode_id = e.id
           left join download d on d.episode_id = e.id and d.state = 'complete'
           where e.id = ?""",
        id,
    ) {
        val path = it.stringOrNull("path")?.let { path -> File(path).takeIf(File::isAbsolute) ?: File(root, path) }
        PlayableEpisode(
            id = id,
            podcastId = it.string("podcast_id"),
            title = it.string("title"),
            podcastTitle = it.string("podcast_title"),
            artworkUrl = it.stringOrNull("artwork"),
            durationMs = it.longOrNull("duration_ms"),
            source = Source.choose(path, it.string("audio_url")),
            stored = Stored(it.long("position_ms"), it.flag("played")),
        )
    }

/** The episode remembered in [LAST_EPISODE_ID], if it still exists locally. One that doesn't is dropped (spec 8). */
fun loadLastEpisode(db: SupportSQLiteDatabase, root: File): PlayableEpisode? {
    val value = db.queryOne("select value from device_setting where key = ?", LAST_EPISODE_ID) { it.string("value") }
        ?: return null
    val id = runCatching { (WireJson.parseToJsonElement(value) as JsonPrimitive).content }.getOrNull() ?: return null
    return loadEpisode(db, id, root)
}

/** Remembers [id] for the next cold start. Device-only, so a plain `device_setting` row. */
fun rememberEpisode(db: SupportSQLiteDatabase, id: String) {
    db.exec(
        "insert into device_setting (key, value) values (?, ?) on conflict (key) do update set value = excluded.value",
        LAST_EPISODE_ID, WireJson.encodeToString(JsonPrimitive.serializer(), JsonPrimitive(id)),
    )
}

/** The local writes for the contract's [writes] on [episode], in order. */
fun changesFor(episode: PlayableEpisode, writes: List<Write>): List<LocalChange> = writes.map { write ->
    val change = when (write) {
        is Write.Position -> EpisodeChange.Position(write.positionMs, write.whilePlaying)
        is Write.Played -> EpisodeChange.Played(write.played)
    }
    LocalChange.Episode(episode.id, episode.podcastId, change)
}

/**
 * The player's writes, made one batch at a time in the order issued: a replay's `played = false`
 * must land before the position saves after it. The worker lives in the process scope, not the
 * service's, so the last position save of a destroyed service still lands.
 */
class PlayerWrites(private val database: PodDatabase, private val sync: SyncHandle, scope: CoroutineScope) {
    private val jobs = Channel<suspend () -> Unit>(Channel.UNLIMITED)

    init {
        scope.launch {
            for (job in jobs) {
                try {
                    withContext(Dispatchers.IO) { job() }
                } catch (e: Exception) {
                    // TODO: tell the user, once the app has a place for errors.
                    Log.e("PodPlayer", "player write failed", e)
                }
            }
        }
    }

    fun write(changes: List<LocalChange>) {
        if (changes.isNotEmpty()) jobs.trySend { writeLocal(database, sync, changes) }
    }

    fun remember(id: String) {
        jobs.trySend { database.runInTransaction { rememberEpisode(database.openHelper.writableDatabase, id) } }
    }
}
