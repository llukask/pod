package ltd.kilian.pod.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.DownloadDone
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Remove
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import ltd.kilian.pod.downloads.Act
import ltd.kilian.pod.downloads.DownloadSettings
import ltd.kilian.pod.downloads.Entry
import ltd.kilian.pod.downloads.Status
import ltd.kilian.pod.downloads.WaitReason
import ltd.kilian.pod.downloads.actions
import ltd.kilian.pod.downloads.formatBytes
import ltd.kilian.pod.downloads.loadSettings
import ltd.kilian.pod.downloads.observeEntries
import ltd.kilian.pod.downloads.perform
import ltd.kilian.pod.downloads.sizeLabel
import ltd.kilian.pod.downloads.sourceLabel
import ltd.kilian.pod.downloads.status
import ltd.kilian.pod.downloads.statusLabel
import ltd.kilian.pod.downloads.undo
import ltd.kilian.pod.store.observe

// ==============================================================================================
// Downloads (wireframe M9) and the download controls on episode rows (M2, M5, M7)
// ==============================================================================================
//
// The reconciler's view of this device (spec 7.4). The screen only writes wishes, rows and the
// archived flag through `downloads.perform`; the reconciler does the rest. Cancelling or removing
// an auto download archives the episode, with an "Archived and removed" undo, so the rule doesn't
// fetch it again.

/** Every wished or downloaded episode's status, by episode id; collected once by the shell. */
val LocalDownloadStatuses = staticCompositionLocalOf<Map<String, Status>> { emptyMap() }

/** Runs [act] for an episode and offers the undo an archive gets. */
@Composable
fun rememberDownloadAction(): (episodeId: String, podcastId: String, act: Act) -> Unit {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val scope = rememberCoroutineScope()
    return remember {
        { episodeId, podcastId, act ->
            scope.launch {
                val removed = perform(pod.database, pod.sync, pod.reconciler, episodeId, podcastId, act)
                if (removed != null) navigator.tell("Archived and removed") { undo(pod.database, pod.sync, pod.reconciler, removed) }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DownloadsScreen() {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val entries by remember { observeEntries(pod.database) }.collectAsState(initial = null)
    val fit by pod.reconciler.fit.collectAsState()
    val progress by pod.reconciler.progress.collectAsState()
    val settings by remember { pod.database.observe("device_setting") { loadSettings(it) } }.collectAsState(initial = DownloadSettings())
    val now = rememberNow(1_000)
    val act = rememberDownloadAction()

    Column(Modifier.fillMaxSize()) {
        TopAppBar(title = { Text("Downloads") })
        val list = entries ?: return@Column
        val rows = list.map { it to status(it, fit, progress) }
        // Complete files and partial ones alike: everything the budget counts (spec 7.4).
        val onDisk = list.sumOf { it.row?.bytesDone ?: 0L }
        val storageFull = rows.any { (_, s) -> s is Status.Queued && s.reason == WaitReason.Space }

        LazyColumn(Modifier.fillMaxSize()) {
            item { StorageMeter(onDisk, settings, onClick = { navigator.tab(Screen.Tab.Settings) }) }
            if (fit.pinsOverBudget || storageFull) {
                item {
                    Card(Modifier.padding(16.dp), colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer)) {
                        Column(Modifier.padding(12.dp)) {
                            Text(
                                if (storageFull) "Storage full, downloads paused" else "Pinned downloads exceed your ${formatBytes(settings.budget)} budget",
                                style = MaterialTheme.typography.titleSmall,
                            )
                            if (!storageFull) Text("Auto downloads are off until you remove pins or raise the budget.")
                            TextButton(onClick = { navigator.tab(Screen.Tab.Settings) }) { Text("Change budget") }
                        }
                    }
                }
            }
            if (list.isEmpty()) {
                item { Text("Nothing downloaded. Tap the download button on an episode, or turn on auto-download for a show.", Modifier.padding(16.dp)) }
            }
            val sections = listOf(
                "In progress" to rows.filter { (_, s) -> s is Status.Queued || s is Status.Downloading || s is Status.Retrying },
                "Failed" to rows.filter { (_, s) -> s is Status.Failed },
                "Skipped" to rows.filter { (_, s) -> s is Status.Skipped },
                "Downloaded" to rows.filter { (_, s) -> s is Status.Downloaded },
            )
            for ((title, section) in sections) {
                if (section.isEmpty()) continue
                item { Text(title, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary, modifier = Modifier.padding(start = 16.dp, top = 16.dp)) }
                items(section, key = { it.first.episodeId }) { (entry, s) ->
                    DownloadRow(entry, s, now, onOpen = { navigator.open(Screen.Episode(entry.episodeId)) }) { act(entry.episodeId, entry.podcastId, it) }
                    HorizontalDivider()
                }
            }
        }
    }
}

/** Everything on disk, partial files included, against the budget (M9 annotation 1). */
@Composable
private fun StorageMeter(onDisk: Long, settings: DownloadSettings, onClick: () -> Unit) {
    Column(Modifier.padding(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("${formatBytes(onDisk)} of ${formatBytes(settings.budget)}", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
            TextButton(onClick = onClick) { Text(if (settings.onMetered) "Mobile data allowed" else "Wi-Fi only") }
        }
        ProgressBar(progress = { (onDisk.toFloat() / settings.budget.coerceAtLeast(1)).coerceIn(0f, 1f) }, Modifier.fillMaxWidth())
    }
}

@Composable
private fun DownloadRow(entry: Entry, status: Status, now: Long, onOpen: () -> Unit, onAct: (Act) -> Unit) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        StatusIcon(status)
        Spacer(Modifier.size(12.dp))
        Column(Modifier.weight(1f)) {
            TextButton(onClick = onOpen, contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp)) {
                Text(entry.title, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            Text(
                listOf(entry.podcastTitle, statusLabel(status, now), sizeLabel(entry), sourceLabel(entry)).joinToString(" · "),
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (status is Status.Downloading && status.total != null && status.total > 0) {
                Spacer(Modifier.height(4.dp))
                ProgressBar(progress = { status.done.toFloat() / status.total }, Modifier.fillMaxWidth())
            }
        }
        actions(status).firstOrNull()?.let { TextButton(onClick = { onAct(it) }) { Text(it.label) } }
    }
}

/** The legend's glyph for a status (M2 annotation 3). */
@Composable
private fun StatusIcon(status: Status?) {
    val size = Modifier.size(20.dp)
    when (status) {
        null -> Unit
        is Status.Queued -> Icon(Icons.Outlined.Schedule, "Queued", size)
        is Status.Downloading -> {
            val fraction = status.total?.takeIf { it > 0 }?.let { status.done.toFloat() / it }
            if (fraction != null) CircularProgressIndicator(progress = { fraction }, size, strokeWidth = 2.dp) else CircularProgressIndicator(size, strokeWidth = 2.dp)
        }
        is Status.Retrying -> Icon(Icons.Outlined.Refresh, "Waiting to retry", size)
        is Status.Failed -> Icon(Icons.Outlined.ErrorOutline, "Failed", size, tint = MaterialTheme.colorScheme.error)
        Status.Skipped -> Icon(Icons.Outlined.Remove, "Skipped, storage budget", size)
        is Status.Downloaded -> Icon(Icons.Outlined.DownloadDone, "Downloaded", size)
    }
}

/** The download glyph at the end of an episode row. Nothing for an episode without a download. */
@Composable
fun DownloadGlyph(episodeId: String) {
    Box(Modifier.padding(start = 8.dp)) { StatusIcon(LocalDownloadStatuses.current[episodeId]) }
}

/**
 * The episode screen's download button (M7 annotation 2): "Download" pins an episode nothing
 * wishes yet; otherwise it shows the status and offers that state's ways out.
 */
@Composable
fun DownloadButton(episodeId: String, podcastId: String) {
    val status = LocalDownloadStatuses.current[episodeId]
    val act = rememberDownloadAction()
    val now = rememberNow(1_000)
    var menu by remember { mutableStateOf(false) }
    if (status == null) {
        FilledTonalButton(onClick = { act(episodeId, podcastId, Act.Download) }) {
            Icon(Icons.Outlined.Download, contentDescription = null)
            Text(" Download")
        }
        return
    }
    Box {
        FilledTonalButton(onClick = { menu = true }) {
            StatusIcon(status)
            Text(" " + statusLabel(status, now).substringBefore(" ·").substringBefore(" ("), maxLines = 1)
        }
        DropdownMenu(menu, onDismissRequest = { menu = false }) {
            DropdownMenuItem(text = { Text(statusLabel(status, now)) }, onClick = {}, enabled = false)
            actions(status).forEach { option ->
                DropdownMenuItem(text = { Text(option.label) }, onClick = {
                    menu = false
                    act(episodeId, podcastId, option)
                })
            }
        }
    }
}

/** The ids of episodes with a finished download, for the podcast page's "Downloaded" filter. */
@Composable
fun rememberDownloadedEpisodes(): Set<String> {
    val statuses = LocalDownloadStatuses.current
    return remember(statuses) { statuses.filterValues { it is Status.Downloaded }.keys }
}
