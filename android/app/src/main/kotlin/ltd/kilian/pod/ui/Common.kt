package ltd.kilian.pod.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import ltd.kilian.pod.store.long
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.store.queryOne
import ltd.kilian.pod.sync.EngineState
import ltd.kilian.pod.sync.monotonicMs
import ltd.kilian.pod.sync.statusLabel

/** The wall clock, ticking every [periodMs], for labels such as "Synced 2 min ago" and "Today". */
@Composable
fun rememberNow(periodMs: Long = 30_000): Long {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(periodMs) {
        while (true) {
            delay(periodMs)
            now = System.currentTimeMillis()
        }
    }
    return now
}

/** How many local writes the server hasn't seen yet. */
@Composable
fun rememberOutboxCount(): Long {
    val pod = LocalPod.current
    val count by remember { pod.database.observe("outbox") { db -> db.queryOne("select count(*) as n from outbox") { it.long("n") }!! } }
        .collectAsState(initial = 0L)
    return count
}

/**
 * The sync status (spec 5.6) beside a page title, never a blocking spinner: a dot (accent when
 * everything is on the server, amber otherwise) and the status in words. Tapping it opens
 * Settings, or the password sheet when signed out.
 */
@Composable
fun SyncChip(modifier: Modifier = Modifier) {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val signIn = LocalSignIn.current
    val state by pod.engine.state.collectAsState()
    val waiting = rememberOutboxCount()
    // A backoff counts down by the second; the rest change by the minute.
    val now = rememberNow(if (state is EngineState.BackingOff) 1_000 else 30_000)
    val settled = state is EngineState.Idle && waiting == 0L
    Row(
        modifier
            .clip(CircleShape)
            .clickable(role = Role.Button) { if (state == EngineState.SignedOut) signIn() else navigator.tab(Screen.Tab.Settings) }
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(statusLabel(state, waiting, monotonicMs(), now), waiting = !settled)
    }
}

/**
 * A determinate bar: a thin rounded track with the accent's fill, as the web's scrubber and the
 * storage meter draw it.
 */
@Composable
fun ProgressBar(progress: () -> Float, modifier: Modifier = Modifier, color: Color = podColors.accent, track: Color = podColors.ink.copy(alpha = 0.12f)) {
    Canvas(modifier.height(4.dp)) {
        val radius = CornerRadius(size.height / 2)
        drawRoundRect(track, cornerRadius = radius)
        val fill = progress().coerceIn(0f, 1f)
        if (fill > 0f) drawRoundRect(color, size = Size(size.width * fill, size.height), cornerRadius = radius)
    }
}
