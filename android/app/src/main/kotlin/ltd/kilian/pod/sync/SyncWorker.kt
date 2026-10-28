package ltd.kilian.pod.sync

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import ltd.kilian.pod.notify.episodesToNotify
import ltd.kilian.pod.notify.postNewEpisodes
import ltd.kilian.pod.pod

/**
 * The background trigger (spec 5.6): an hourly sync while the app isn't running, when there is
 * a network, followed by the new-episode notification for what it brought in (7.6). It runs in the app's process, so it shares the engine and its mutex with the
 * foreground driver; the unique work name keeps a second one from being queued.
 */
class SyncWorker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    // A failed request isn't retried here: the next hourly run, or the driver once the app is
    // open, picks it up.
    override suspend fun doWork(): Result {
        val pod = applicationContext.pod
        val synced = pod.engine.syncReporting()
        val episodes = withContext(Dispatchers.IO) {
            episodesToNotify(pod.database.openHelper.readableDatabase, synced.inserted, System.currentTimeMillis())
        }
        postNewEpisodes(applicationContext, episodes)
        return Result.success()
    }

    companion object {
        private const val NAME = "sync"

        fun schedule(context: Context) {
            val request = PeriodicWorkRequestBuilder<SyncWorker>(1, TimeUnit.HOURS)
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .build()
            WorkManager.getInstance(context).enqueueUniquePeriodicWork(NAME, ExistingPeriodicWorkPolicy.KEEP, request)
        }
    }
}
