package ltd.kilian.pod.notify

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.sqlite.db.SupportSQLiteDatabase
import ltd.kilian.pod.MainActivity
import ltd.kilian.pod.R
import ltd.kilian.pod.inbox.displayTitle
import ltd.kilian.pod.store.queryList
import ltd.kilian.pod.store.string
import ltd.kilian.pod.store.stringOrNull

// ==============================================================================================
// New-episode notifications (spec 7.6, wireframe M12)
// ==============================================================================================
//
// After a background sync, the episodes that sync inserted for podcasts with
// `podcast_device.notify = 1` and published within the last 48 hours produce one grouped
// notification. Tapping it opens the inbox. The foreground app doesn't notify: the user is
// looking at the inbox already.

/** How recent an episode must be to notify about it, so a backfill never floods the shade. */
const val NOTIFY_WINDOW_MS = 48 * 3_600_000L

private const val CHANNEL = "new_episodes"
private const val NOTIFICATION_ID = 1

/** An episode to notify about. */
data class NewEpisode(val podcastTitle: String, val title: String)

/** Of [inserted], the episodes the notification names, newest first. */
fun episodesToNotify(db: SupportSQLiteDatabase, inserted: Set<String>, now: Long): List<NewEpisode> {
    if (inserted.isEmpty()) return emptyList()
    // The set is one sync's worth of new episodes; an `in` list of that size is fine for SQLite.
    val placeholders = inserted.joinToString(",") { "?" }
    return db.queryList(
        """select e.title, p.title as podcast_title, p.local_title, p.feed_url
           from episode e
           join podcast p on p.id = e.podcast_id
           join podcast_device d on d.podcast_id = e.podcast_id and d.notify = 1
           where e.id in ($placeholders) and e.published_at >= ?
           order by e.published_at desc, e.id desc""",
        *inserted.toTypedArray(), now - NOTIFY_WINDOW_MS,
    ) { NewEpisode(displayTitle(it.stringOrNull("podcast_title"), it.stringOrNull("local_title"), it.string("feed_url")), it.string("title")) }
}

/** The notification's title: the one episode, or how many there are. */
fun notificationTitle(episodes: List<NewEpisode>): String =
    if (episodes.size == 1) episodes.single().podcastTitle else "${episodes.size} new episodes"

/** Posts or replaces the grouped notification. Does nothing without the permission or episodes. */
fun postNewEpisodes(context: Context, episodes: List<NewEpisode>) {
    if (episodes.isEmpty()) return
    if (context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return
    val manager = context.getSystemService(NotificationManager::class.java)
    manager.createNotificationChannel(NotificationChannel(CHANNEL, "New episodes", NotificationManager.IMPORTANCE_DEFAULT))

    val open = PendingIntent.getActivity(
        context, 0,
        Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
        PendingIntent.FLAG_IMMUTABLE,
    )
    val lines = NotificationCompat.InboxStyle()
    episodes.take(6).forEach { lines.addLine("${it.podcastTitle}: ${it.title}") }
    if (episodes.size > 6) lines.setSummaryText("and ${episodes.size - 6} more")
    val notification = NotificationCompat.Builder(context, CHANNEL)
        .setSmallIcon(R.drawable.ic_notification)
        .setContentTitle(notificationTitle(episodes))
        .setContentText(if (episodes.size == 1) episodes.single().title else episodes.joinToString(", ") { it.podcastTitle }.take(120))
        .setStyle(lines)
        .setNumber(episodes.size)
        .setContentIntent(open)
        .setAutoCancel(true)
        .build()
    NotificationManagerCompat.from(context).notify(NOTIFICATION_ID, notification)
}
