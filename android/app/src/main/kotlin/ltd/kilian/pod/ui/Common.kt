package ltd.kilian.pod.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudDone
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Podcasts
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
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
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
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
 * The sync status (spec 5.6) in the app bar, never a blocking spinner. Tapping it opens
 * Settings, or the password sheet when signed out.
 */
@Composable
fun SyncChip() {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val signIn = LocalSignIn.current
    val state by pod.engine.state.collectAsState()
    val waiting = rememberOutboxCount()
    // A backoff counts down by the second; the rest change by the minute.
    val now = rememberNow(if (state is EngineState.BackingOff) 1_000 else 30_000)
    val icon = when (state) {
        is EngineState.Idle -> Icons.Outlined.CloudDone
        is EngineState.Running -> Icons.Outlined.Sync
        EngineState.Offline -> Icons.Outlined.CloudOff
        is EngineState.BackingOff, EngineState.SignedOut -> Icons.Outlined.ErrorOutline
    }
    AssistChip(
        onClick = { if (state == EngineState.SignedOut) signIn() else navigator.tab(Screen.Tab.Settings) },
        label = {
            Text(
                statusLabel(state, waiting, monotonicMs(), now),
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        },
        leadingIcon = { Icon(icon, contentDescription = null, Modifier.size(18.dp)) },
    )
}

/** A podcast or episode image from the network, cached on disk by Coil (spec 7.5), with a glyph until it loads. */
@Composable
fun Artwork(url: String?, size: Dp, modifier: Modifier = Modifier) {
    val shape = RoundedCornerShape(size / 8)
    Box(
        modifier.size(size).clip(shape).background(MaterialTheme.colorScheme.surfaceVariant),
        contentAlignment = Alignment.Center,
    ) {
        Icon(Icons.Outlined.Podcasts, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        if (url != null) AsyncImage(url, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.size(size))
    }
}
