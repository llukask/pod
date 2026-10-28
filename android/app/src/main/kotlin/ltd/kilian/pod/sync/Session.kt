package ltd.kilian.pod.sync

import androidx.sqlite.db.SupportSQLiteDatabase
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.exec

/**
 * Logout's table wipe (spec 5.7): every table except `device_setting`, which belongs to the
 * device. That includes `setting`, the user's synced settings, and `podcast_device`, which
 * belongs to that user's podcasts. Unsent outbox rows are lost; the logout dialog says so.
 *
 * TODO: the rest of logout (taking the sync mutex, cancelling downloads, deleting the downloads
 * directory, `POST /v1/auth/logout`) with the engine and the reconciler.
 */
fun deleteAccountData(db: SupportSQLiteDatabase) {
    PodDatabase.TABLES.filter { it != "device_setting" }.forEach { db.exec("delete from $it") }
}
