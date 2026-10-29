package ltd.kilian.pod

import android.app.Application
import android.content.Context
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import ltd.kilian.pod.downloads.Reconciler
import ltd.kilian.pod.downloads.Transfer
import ltd.kilian.pod.downloads.WorkManagerJobs
import ltd.kilian.pod.downloads.downloadHttpClient
import ltd.kilian.pod.net.NetworkMonitor
import ltd.kilian.pod.net.podHttpClient
import ltd.kilian.pod.player.PlayerWrites
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.sync.Session
import ltd.kilian.pod.sync.SyncEngine
import ltd.kilian.pod.sync.SyncHandle
import ltd.kilian.pod.sync.SyncWorker

/**
 * The process-wide services: one store, one sync engine with its mutex, one session. Activities,
 * the playback service and WorkManager jobs reach them through [Context.pod], so they all share
 * the mutex spec 5.6 asks for.
 */
class Pod(context: Context) {
    val database = PodDatabase.open(context)
    val network = NetworkMonitor(context)
    val sync = SyncHandle()
    val engine = SyncEngine(database, sync, podHttpClient(), network.state)

    /** Downloads under `getExternalFilesDir("audio")` (spec 7.1), app storage while that is unavailable. */
    val reconciler = Reconciler(
        database,
        Transfer(database, downloadHttpClient(engine.http), context.getExternalFilesDir("audio") ?: File(context.filesDir, "audio"), { network.state.value }),
        network.state,
        WorkManagerJobs(context),
    )
    val session = Session(database, engine) { wipe ->
        reconciler.stopped(timeoutMs = 10_000) {
            wipe()
            withContext(Dispatchers.IO) { reconciler.root.deleteRecursively() }
        }
    }

    /** Lives as long as the process. */
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** The playback service's store writes, on the process scope so the last one outlives the service. */
    val playerWrites = PlayerWrites(database, sync, scope)

    fun start(context: Context) {
        scope.launch { engine.run() }
        scope.launch { reconciler.run(engine.loopEnded) }
        SyncWorker.schedule(context)
    }
}

class PodApp : Application() {
    lateinit var pod: Pod
        private set

    override fun onCreate() {
        super.onCreate()
        pod = Pod(this)
        pod.start(this)
    }
}

val Context.pod: Pod get() = (applicationContext as PodApp).pod
