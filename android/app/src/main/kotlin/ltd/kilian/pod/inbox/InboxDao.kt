package ltd.kilian.pod.inbox

import androidx.room.Dao
import androidx.room.Query
import kotlinx.coroutines.flow.Flow
import kotlinx.serialization.json.Json
import ltd.kilian.pod.core.Settings
import ltd.kilian.pod.store.EpisodeRow

/**
 * The inbox (spec 6): unplayed, unarchived episodes of subscribed podcasts published inside the
 * window, newest first. An episode published before the user subscribed counts only if it is its
 * podcast's newest, so subscribing to a show puts its latest episode in the inbox without its
 * back catalogue. Ids compare as their lowercase strings, which is SQLite's text order.
 * `conformance/inbox.json` pins the rules.
 */
@Dao
interface InboxDao {
    @Query(INBOX)
    fun inbox(windowStart: Long): List<EpisodeRow>

    @Query(INBOX)
    fun observeInbox(windowStart: Long): Flow<List<EpisodeRow>>

    @Query("select value from setting where `key` = '${Settings.INBOX_WINDOW_DAYS}'")
    fun inboxWindowSetting(): String?

    companion object {
        const val INBOX = """
            select e.* from episode e
            join subscription s on s.podcast_id = e.podcast_id and s.subscribed = 1
            left join episode_state st on st.episode_id = e.id
            where coalesce(st.played, 0) = 0
              and coalesce(st.archived, 0) = 0
              and e.published_at >= :windowStart
              and (
                    e.published_at >= s.subscribed_at
                 or e.id = (select e2.id from episode e2
                            where e2.podcast_id = e.podcast_id
                            order by e2.published_at desc, e2.id desc limit 1)
              )
            order by e.published_at desc, e.id desc"""

        /** `now - inbox_window_days`, with the stored setting read the way the server reads it. */
        fun windowStart(now: Long, storedSetting: String?): Long {
            // Unparseable JSON is as unusable as a wrong type: the default applies.
            val stored = storedSetting?.let { runCatching { Json.parseToJsonElement(it) }.getOrNull() }
            return now - Settings.inboxWindowDays(stored) * Settings.DAY_MS
        }
    }
}
