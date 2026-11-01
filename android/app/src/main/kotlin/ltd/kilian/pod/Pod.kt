package ltd.kilian.pod

import android.app.Application
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import ltd.kilian.pod.connect.ConnectController
import ltd.kilian.pod.connect.HttpConnectServer
import ltd.kilian.pod.downloads.Reconciler
import ltd.kilian.pod.downloads.Transfer
import ltd.kilian.pod.downloads.WorkManagerJobs
import ltd.kilian.pod.downloads.downloadHttpClient
import ltd.kilian.pod.net.NetworkMonitor
import ltd.kilian.pod.net.podHttpClient
import ltd.kilian.pod.player.PlaybackService
import ltd.kilian.pod.player.PlayerWrites
import ltd.kilian.pod.store.PodDatabase
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.store.string
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
    /** Lives as long as the process. */
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** The playback service's store writes, on the process scope so the last one outlives the service. */
    val playerWrites = PlayerWrites(database, sync, scope)

    /**
     * Connect (spec 14): the event stream, held while an activity is visible or the playback
     * service exists, and the remote control of whichever device plays.
     */
    val connect = ConnectController(
        HttpConnectServer(database, engine.http),
        deviceId = database.invalidationTracker.createFlow("account").map {
            withContext(Dispatchers.IO) {
                database.openHelper.readableDatabase.queryOne("select device_id from account where id = 1") { it.string("device_id") }
            }
        }.distinctUntilChanged(),
        foreground = ProcessLifecycleOwner.get().lifecycle.currentStateFlow.map { it.isAtLeast(Lifecycle.State.STARTED) },
        connected = network.state.map { it.connected },
        flushWrites = {
            playerWrites.flushed()
            engine.sync()
        },
        requestSync = { sync.requestSync(0) },
        startPlayer = {
            try {
                context.startService(Intent(context, PlaybackService::class.java))
            } catch (e: IllegalStateException) {
                // Not allowed from the background; the event waits for the service, or goes stale.
                Log.w("PodConnect", "couldn't start the playback service for an event", e)
            }
        },
        scope = scope,
    )

    val session = Session(database, engine) { wipe ->
        connect.signedOut()
        reconciler.stopped(timeoutMs = 10_000) {
            wipe()
            withContext(Dispatchers.IO) { reconciler.root.deleteRecursively() }
        }
    }

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
