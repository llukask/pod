package ltd.kilian.pod.connect

import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withContext
import ltd.kilian.pod.core.ConnectCommand
import ltd.kilian.pod.core.ConnectDevice
import ltd.kilian.pod.core.ConnectEvent
import ltd.kilian.pod.core.ConnectReport
import ltd.kilian.pod.core.ConnectRole
import ltd.kilian.pod.core.ConnectState
import ltd.kilian.pod.core.DeviceKind
import ltd.kilian.pod.core.Playback
import ltd.kilian.pod.core.PlaybackEpisode
import ltd.kilian.pod.core.PlayerAction
import ltd.kilian.pod.core.SonosPlayer
import ltd.kilian.pod.core.SonosPlayerState
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The Connect controller against a scripted server: the event stream is a channel the test
 * feeds, and every request is logged. The pure rules it applies are `core`'s `ConnectTest`; what
 * the playback service does with the actions is the service's.
 */
@RunWith(RobolectricTestRunner::class)
class ConnectControllerTest {
    private val episode = "0192f0a0-0000-7000-8000-000000000001"
    private val other = "0192f0a0-0000-7000-8000-000000000005"
    private val podcast = "0192f0a0-0000-7000-8000-000000000002"
    private val phone = "0192f0a1-0000-7000-8000-000000000003"
    private val laptop = "0192f0a1-0000-7000-8000-000000000004"
    private val couch = "RINCON_804AF246458401400"
    private val devices = listOf(ConnectDevice(phone, "Pixel 8", DeviceKind.Android), ConnectDevice(laptop, "Laptop", DeviceKind.Desktop))

    private fun playback(deviceId: String? = laptop, positionMs: Long = 90_000, playing: Boolean = true, volume: Int? = null) =
        Playback(deviceId, PlaybackEpisode(episode, podcast, "Episode", "Show", null, 3_600_000), positionMs, playing, 1.0, volume)

    /** pod-server, scripted. */
    private class Server : ConnectServer {
        /** Every request as "kind body", in order. */
        val log: MutableList<String> = Collections.synchronizedList(mutableListOf())

        /** The events of the open stream, in order; null closes it. */
        val stream = Channel<ConnectEvent?>(Channel.UNLIMITED)
        val opens = AtomicInteger()

        /** How the next streams end at once, instead of opening. */
        @Volatile var refuseStream: StreamEnd? = null

        /** What every request fails with, while set. */
        @Volatile var failWith: ConnectException? = null

        /** The playback the requests answer with, and whether a command ran (`200`) or was passed on (`202`). */
        @Volatile var answer: Playback? = null
        @Volatile var applied = false
        @Volatile var players = emptyList<SonosPlayer>()

        /** Holds the next command or transfer until counted down. */
        @Volatile var gate: CountDownLatch? = null
        val requestArrived = CountDownLatch(1)

        override suspend fun events(onOpen: suspend () -> Unit, onEvent: suspend (ConnectEvent) -> Unit): StreamEnd {
            opens.incrementAndGet()
            refuseStream?.let { return it }
            onOpen()
            for (event in stream) onEvent(event ?: return StreamEnd.Closed)
            return StreamEnd.Closed
        }

        /** Answers a request. A gate holds it the way a slow server would, suspended, not blocking the caller's thread. */
        private suspend fun respond(entry: String): Playback {
            log += entry
            requestArrived.countDown()
            gate?.let { held ->
                gate = null
                withContext(Dispatchers.IO) { held.await(5, TimeUnit.SECONDS) }
            }
            failWith?.let { throw it }
            return answer ?: error("no answer scripted for $entry")
        }

        override suspend fun report(report: ConnectReport) = respond("report $report")

        override suspend fun command(command: ConnectCommand) = CommandAnswer(respond("command $command"), applied)

        override suspend fun transfer(deviceId: String) = respond("transfer $deviceId")

        override suspend fun state(): ConnectState {
            log += "state"
            return ConnectState(emptyList(), answer)
        }

        override suspend fun players(): List<SonosPlayer> {
            log += "players"
            return players
        }
    }

    private val server = Server()
    private val deviceId = MutableStateFlow<String?>(phone)
    private val foreground = MutableStateFlow(true)
    private val online = MutableStateFlow(true)
    private val syncs = AtomicInteger()
    private val starts = AtomicInteger()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val connect = ConnectController(
        server,
        deviceId = deviceId,
        foreground = foreground,
        connected = online,
        flushWrites = { server.log += "flush" },
        requestSync = { syncs.incrementAndGet() },
        startPlayer = { starts.incrementAndGet() },
        scope = scope,
    )

    @After
    fun close() {
        scope.cancel()
    }

    private fun waitFor(what: String, timeoutMs: Long = 5_000, condition: () -> Boolean) {
        val until = System.currentTimeMillis() + timeoutMs
        while (!condition()) {
            check(System.currentTimeMillis() < until) { "timed out waiting for $what; view ${connect.view.value}, log ${server.log}" }
            Thread.sleep(5)
        }
    }

    private val view get() = connect.view.value

    /** Opens the stream and delivers a `state` with [playback]. */
    private fun streaming(playback: Playback?) {
        waitFor("the stream") { view.streaming }
        server.stream.trySend(ConnectEvent.State(devices, playback))
        waitFor("the state") { view.seen?.playback == playback && view.devices == devices }
    }

    private fun commandsSent(): List<String> = server.log.filter { it.startsWith("command") }

    /** The next snackbar message, subscribed before the caller acts so none is missed. */
    private fun CoroutineScope.nextMessage() = async(start = CoroutineStart.UNDISPATCHED) { withTimeout(5_000) { connect.messages.first() } }

    // ==========================================================================================
    // The stream
    // ==========================================================================================

    @Test
    fun theStreamIsHeldWhileAnActivityIsVisibleOrThePlayerExistsAndOnline() {
        waitFor("the stream") { view.streaming }
        foreground.value = false
        waitFor("closed in the background") { !view.streaming }
        connect.serviceStarted()
        waitFor("held for the service") { view.streaming }
        online.value = false
        waitFor("closed offline") { !view.streaming }
        online.value = true
        waitFor("held again") { view.streaming }
        deviceId.value = null
        waitFor("closed after sign-out") { !view.streaming }
        assertEquals(3, server.opens.get())
    }

    @Test
    fun aStateMakesThisDeviceTheRemoteOfAnotherOne() {
        streaming(playback(laptop))
        assertEquals(Output.Remote(Target(laptop, "Laptop", sonos = false)), view.output)
        assertEquals(ConnectRole.Remote, view.role)
        assertEquals(listOf(devices[1]), view.others)
    }

    @Test
    fun aSonosGroupIsNamedByThePlayerList() {
        server.players = listOf(SonosPlayer(couch, "Couch", 10, SonosPlayerState.Pod))
        streaming(playback(couch, volume = 10))
        waitFor("the group's name") { (view.output as? Output.Remote)?.target == Target(couch, "Couch", sonos = true) }
        assertEquals(10, view.volume)
        assertTrue(view.sonosAvailable)
    }

    @Test
    fun aClosedStreamReconnectsAfterASecond() {
        streaming(playback(laptop))
        server.stream.trySend(null)
        waitFor("closed") { !view.streaming }
        assertNull("the playback is unknown without a stream", view.seen)
        assertEquals(Output.Local, view.output)
        waitFor("the reconnect", timeoutMs = 3_000) { server.opens.get() == 2 && view.streaming }
    }

    @Test
    fun aReplacedStreamDoesntReconnect() {
        streaming(null)
        server.stream.trySend(ConnectEvent.Replaced)
        server.stream.trySend(null)
        waitFor("closed") { !view.streaming }
        Thread.sleep(1_500)
        assertEquals(1, server.opens.get())
    }

    @Test
    fun aSignedOutDeviceStopsAndLetsTheEngineFindOut() {
        foreground.value = false
        waitFor("no stream") { !view.streaming }
        val opened = server.opens.get()
        server.refuseStream = StreamEnd.Unauthorized
        foreground.value = true
        waitFor("the sync request") { syncs.get() == 1 }
        Thread.sleep(1_500)
        assertEquals(opened + 1, server.opens.get())
        // Signing in again changes the device id, which holds the stream again.
        server.refuseStream = null
        deviceId.value = "0192f0a1-0000-7000-8000-000000000009"
        waitFor("the stream") { view.streaming }
    }

    // ==========================================================================================
    // Events for the local player
    // ==========================================================================================

    @Test
    fun eventsForTheLocalPlayerGoToTheServiceInOrderAndStartIt() = runBlocking {
        streaming(playback(phone))
        server.stream.trySend(ConnectEvent.Command(ConnectCommand.Pause))
        server.stream.trySend(ConnectEvent.Load(other, 101_500))
        server.stream.trySend(ConnectEvent.Release("t-1"))
        val received = List(3) { withTimeout(5_000) { connect.actions.receive().action } }
        assertEquals(listOf(PlayerAction.Pause, PlayerAction.StartAt(other, 101_500), PlayerAction.Release("t-1")), received)
        assertTrue("the service is started for an event while it isn't running", starts.get() > 0)
        connect.serviceStarted()
        val before = starts.get()
        server.stream.trySend(ConnectEvent.Command(ConnectCommand.Resume))
        assertEquals(PlayerAction.Resume, withTimeout(5_000) { connect.actions.receive().action })
        assertEquals(before, starts.get())
    }

    // ==========================================================================================
    // Reports
    // ==========================================================================================

    @Test
    fun reportsGoOutOnlyWhileTheStreamIsOpenAndTheirAnswerSaysWhoPlays() {
        foreground.value = false
        waitFor("no stream") { !view.streaming }
        connect.report(ConnectReport(episode, 1_000, true, 1.0))
        Thread.sleep(100)
        assertEquals(emptyList<String>(), server.log)

        foreground.value = true
        streaming(playback(laptop))
        server.answer = playback(phone, positionMs = 2_000)
        connect.report(ConnectReport(episode, 2_000, true, 1.0))
        waitFor("this device active") { view.role == ConnectRole.Active }
        assertEquals(Output.Local, view.output)
        assertEquals(listOf("report ${ConnectReport(episode, 2_000, true, 1.0)}"), server.log)
    }

    @Test
    fun aReportWithoutAStreamOnTheServerIsDropped() = runBlocking {
        streaming(playback(phone))
        server.failWith = ConnectException(409, "no_session", "no stream")
        val message = async(start = CoroutineStart.UNDISPATCHED) { withTimeout(500) { connect.messages.first() } }
        connect.report(ConnectReport(episode, 2_000, true, 1.0))
        waitFor("the report") { server.log.isNotEmpty() }
        assertTrue(runCatching { message.await() }.isFailure)
        assertEquals(ConnectRole.Active, view.role)
    }

    // ==========================================================================================
    // Remote control
    // ==========================================================================================

    @Test
    fun theRemoteSendsCommandsInOrderAndShowsThemAtOnce() {
        streaming(playback(laptop))
        server.answer = playback(laptop)
        val gate = CountDownLatch(1)
        server.gate = gate
        connect.toggle()
        waitFor("shown paused before the device confirms") { view.seen?.playback?.playing == false }
        connect.skip(-15_000)
        connect.seek(600_000)
        connect.play(other)
        gate.countDown()
        waitFor("four commands") { commandsSent().size == 4 }
        assertEquals(
            listOf(ConnectCommand.Pause, ConnectCommand.Skip(-15_000), ConnectCommand.Seek(600_000), ConnectCommand.Play(other)).map { "command $it" },
            commandsSent(),
        )
        // A client device answers 202 with the playback from before; the remote keeps what it shows.
        Thread.sleep(100)
        assertEquals(false, view.seen?.playback?.playing)
        assertEquals(600_000L, view.seen?.playback?.positionMs)
    }

    @Test
    fun theDevicesReportReplacesWhatTheRemoteShows() {
        streaming(playback(laptop))
        server.answer = playback(laptop)
        connect.toggle()
        waitFor("the command") { commandsSent().size == 1 }
        server.stream.trySend(ConnectEvent.State(devices, playback(laptop, positionMs = 91_000, playing = false)))
        waitFor("the reported position") { view.seen?.playback?.positionMs == 91_000L }
    }

    @Test
    fun aCommandThatFailsShowsThePlaybackAsItWasAndSaysSo() = runBlocking {
        streaming(playback(laptop))
        server.failWith = ConnectException(null, null, "pod-server didn't answer")
        val message = nextMessage()
        connect.toggle()
        assertEquals("Couldn't reach Laptop.", message.await())
        waitFor("playing again") { view.seen?.playback?.playing == true }
    }

    @Test
    fun aCommandThatFindsNothingActiveAsksForTheState() {
        streaming(playback(laptop))
        server.failWith = ConnectException(409, "no_session", "nothing plays")
        server.answer = playback(null, playing = false)
        connect.toggle()
        waitFor("the state") { view.role == ConnectRole.Idle }
        assertTrue("state" in server.log)
        assertEquals("the remote ended", 1, syncs.get())
    }

    @Test
    fun withoutAnotherActiveDeviceTheRemoteSendsNothing() {
        streaming(playback(null, playing = false))
        connect.toggle()
        connect.skip(30_000)
        Thread.sleep(100)
        assertEquals(emptyList<String>(), commandsSent())
    }

    @Test
    fun aSonosGroupsAnswerShowsAtOnce() {
        streaming(playback(couch, volume = 10))
        server.answer = playback(couch, positionMs = 120_000, playing = false, volume = 10)
        server.applied = true
        connect.toggle()
        waitFor("the group's answer") { view.seen?.playback?.positionMs == 120_000L }
        assertEquals(false, view.seen?.playback?.playing)
    }

    @Test
    fun aDraggedVolumeSendsOnlyTheNewestValueWhileOneIsOut() {
        streaming(playback(couch, volume = 10))
        server.answer = playback(couch, volume = 10)
        server.applied = true
        val gate = CountDownLatch(1)
        server.gate = gate
        connect.setVolume(20)
        check(server.requestArrived.await(5, TimeUnit.SECONDS)) { "the first volume command never arrived" }
        connect.setVolume(30)
        connect.setVolume(40)
        waitFor("the shown volume") { view.volume == 40 }
        gate.countDown()
        waitFor("the second command") { commandsSent().size == 2 }
        Thread.sleep(100)
        assertEquals(listOf(ConnectCommand.Volume(20), ConnectCommand.Volume(40)).map { "command $it" }, commandsSent())
        // The answers still carry the old volume, as a state sent before the change would.
        assertEquals(40, view.volume)
    }

    @Test
    fun aClientDeviceHasNoVolume() {
        streaming(playback(laptop))
        connect.setVolume(20)
        Thread.sleep(100)
        assertEquals(emptyList<String>(), commandsSent())
        assertNull(view.volume)
    }

    @Test
    fun whenTheActiveDeviceGoesAwayTheRemoteAsksForASync() {
        streaming(playback(laptop))
        server.stream.trySend(ConnectEvent.State(devices, playback(null, playing = false)))
        waitFor("nothing active") { view.role == ConnectRole.Idle }
        assertEquals(1, syncs.get())
        assertEquals(Output.Local, view.output)
        // Only a remote asks: this device playing and going quiet isn't one.
        server.stream.trySend(ConnectEvent.State(devices, playback(phone)))
        server.stream.trySend(ConnectEvent.State(devices, playback(null, positionMs = 95_000, playing = false)))
        waitFor("nothing active") { view.seen?.playback?.positionMs == 95_000L }
        Thread.sleep(50)
        assertEquals(1, syncs.get())
    }

    @Test
    fun aRemoteThatEndedWhileTheStreamWasClosedAsksForASyncOnReconnect() {
        streaming(playback(laptop))
        server.stream.trySend(null)
        waitFor("closed") { !view.streaming }
        waitFor("the reconnect", timeoutMs = 3_000) { view.streaming }
        server.stream.trySend(ConnectEvent.State(devices, playback(null, playing = false)))
        waitFor("the sync request") { syncs.get() == 1 }
    }

    // ==========================================================================================
    // Transfer
    // ==========================================================================================

    @Test
    fun aTransferShowsConnectingAndThenTheTarget() {
        streaming(playback(phone))
        server.answer = playback(laptop, playing = false)
        val gate = CountDownLatch(1)
        server.gate = gate
        connect.transfer(Target(laptop, "Laptop", sonos = false))
        waitFor("connecting") { view.output == Output.Connecting(Target(laptop, "Laptop", sonos = false)) }
        gate.countDown()
        waitFor("the target") { view.output is Output.Remote }
        assertEquals(listOf("transfer $laptop"), server.log)
    }

    @Test
    fun aTransferToAGroupFromNothingActivePushesTheOutboxFirst() {
        streaming(playback(null, playing = false))
        server.answer = playback(couch, volume = 12)
        connect.transfer(Target(couch, "Couch", sonos = true))
        waitFor("the group") { view.output is Output.Remote }
        assertEquals(listOf("flush", "transfer $couch"), server.log.take(2))
        assertEquals(12, view.volume)
    }

    @Test
    fun aTransferThatFailsStaysAndSaysSo() = runBlocking {
        streaming(playback(phone))
        server.failWith = ConnectException(502, "upstream_unavailable", "the speaker didn't answer")
        val message = nextMessage()
        connect.transfer(Target(couch, "Couch", sonos = true))
        assertEquals("Couldn't reach Couch.", message.await())
        waitFor("local again") { view.output == Output.Local }
    }

    @Test
    fun choosingThisDeviceWhileItPlaysSendsNothing() {
        streaming(playback(phone))
        waitFor("self known") { connect.self() != null }
        connect.transfer(connect.self()!!)
        Thread.sleep(100)
        assertEquals(emptyList<String>(), server.log)
        assertFalse(view.output is Output.Connecting)
    }

    @Test
    fun takingThePlaybackBackFromAnotherDeviceTransfersHere() {
        streaming(playback(laptop))
        server.answer = playback(phone, playing = false)
        waitFor("self known") { connect.self() != null }
        connect.transfer(connect.self()!!)
        waitFor("this device active") { view.role == ConnectRole.Active }
        assertEquals(listOf("transfer $phone"), server.log)
    }
}
