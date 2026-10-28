package ltd.kilian.pod.downloads

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import ltd.kilian.pod.pod

private const val EPISODE_ID = "episode_id"

/** Every transfer job carries this tag, and `episode:<id>` names its episode. */
private const val DOWNLOAD_TAG = "download"
private const val EPISODE_TAG = "episode:"

/**
 * One transfer as a WorkManager job (spec 7.4). The job carries only the episode id; the run it
 * belongs to is on the row, which the reconciler left `transferring`.
 *
 * A job WorkManager stops (its network constraint lost, or its execution window over) is
 * cancelled here, checkpoints, and runs again later from that checkpoint under the same run.
 *
 * TODO: a foreground notification (`setForeground`) for long transfers, so an hour-long episode
 * on a slow link isn't stopped and resumed every ten minutes.
 */
class DownloadWorker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
        val episodeId = inputData.getString(EPISODE_ID) ?: return Result.failure()
        // The transfer records its own outcome on the row; the job is done either way.
        applicationContext.pod.reconciler.runTransfer(episodeId)
        return Result.success()
    }
}

/** [TransferJobs] as unique WorkManager jobs named `download:<episode_id>`, expedited for pins. */
class WorkManagerJobs(context: Context) : TransferJobs {
    private val work = WorkManager.getInstance(context)

    override fun start(episodeId: String, expedited: Boolean) {
        val request = OneTimeWorkRequestBuilder<DownloadWorker>()
            .setInputData(workDataOf(EPISODE_ID to episodeId))
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .addTag(DOWNLOAD_TAG)
            .addTag(EPISODE_TAG + episodeId)
            .apply { if (expedited) setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST) }
            .build()
        // A pass only starts rows without a live job, so this never replaces a running one.
        work.enqueueUniqueWork("download:$episodeId", ExistingWorkPolicy.REPLACE, request)
    }

    override fun cancel(episodeId: String) {
        work.cancelUniqueWork("download:$episodeId")
    }

    override fun cancelAll() {
        work.cancelAllWorkByTag(DOWNLOAD_TAG)
    }

    override suspend fun live(): Set<String> = withContext(Dispatchers.IO) {
        work.getWorkInfosByTag(DOWNLOAD_TAG).get()
            .filter { !it.state.isFinished }
            .flatMap { info -> info.tags.filter { it.startsWith(EPISODE_TAG) }.map { it.removePrefix(EPISODE_TAG) } }
            .toSet()
    }
}
