package ltd.kilian.pod.store

import android.content.Context
import androidx.room.Database
import androidx.room.Room
import androidx.room.RoomDatabase

@Database(
    version = 1,
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
            Room.databaseBuilder(context, PodDatabase::class.java, NAME).build()
    }
}
