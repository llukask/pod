package ltd.kilian.pod

import android.app.Application
import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import ltd.kilian.pod.net.NetworkMonitor
import ltd.kilian.pod.net.podHttpClient
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
    val session = Session(database, engine)

    /** Lives as long as the process. */
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    fun start(context: Context) {
        scope.launch { engine.run() }
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
