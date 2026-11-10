package ltd.kilian.pod.store

import android.content.Context
import androidx.room.Database
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.migration.Migration
import androidx.sqlite.db.SupportSQLiteDatabase

@Database(
    version = 2,
    entities = [
        PodcastRow::class, EpisodeRow::class, SubscriptionRow::class, EpisodeStateRow::class,
        SettingRow::class, SyncScopeRow::class, OutboxRow::class, AccountRow::class,
        PodcastDeviceRow::class, DownloadWishRow::class, DownloadRow::class, KnownSizeRow::class,
        DeviceSettingRow::class, RejectedMutationRow::class, ArtworkRow::class,
    ],
)
abstract class PodDatabase : RoomDatabase() {
    companion object {
        /** Distinct from v1's database file, which v2 never opens. */
        const val NAME = "pod-v2.db"

        /** Every table, in the order logout and the fixture runners walk them. */
        val TABLES = listOf(
            "podcast", "episode", "subscription", "episode_state", "setting", "sync_scope", "outbox",
            "account", "podcast_device", "download_wish", "download", "known_size", "device_setting",
            "rejected_mutation", "artwork",
        )

        fun open(context: Context): PodDatabase =
            Room.databaseBuilder(context, PodDatabase::class.java, NAME).addMigrations(MIGRATION_1_2).build()
    }
}

/**
 * Adds `podcast.show_type` and `episode.episode_type` (spec 16.1). Rows pulled before this version
 * dropped both fields, and the server sends a row again only when it changes, so every podcast
 * scope starts over at version 0 and the next sync pulls its rows again (spec 5.1). The user
 * scope keeps its cursor, since it holds no catalog rows.
 */
val MIGRATION_1_2 = object : Migration(1, 2) {
    override fun migrate(db: SupportSQLiteDatabase) {
        db.execSQL("alter table podcast add column show_type TEXT NOT NULL DEFAULT 'episodic'")
        db.execSQL("alter table episode add column episode_type TEXT NOT NULL DEFAULT 'full'")
        db.execSQL("update sync_scope set version = 0, complete = 0 where scope like 'podcast:%'")
    }
}
