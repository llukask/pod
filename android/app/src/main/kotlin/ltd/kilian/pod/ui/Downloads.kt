package ltd.kilian.pod.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Text
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
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
    val gutter = LocalGutter.current

    Column(Modifier.fillMaxSize()) {
        PageHeader("Downloads")
        val list = entries ?: return@Column
        val rows = list.map { it to status(it, fit, progress) }
        // Complete files and partial ones alike: everything the budget counts (spec 7.4).
        val onDisk = list.sumOf { it.row?.bytesDone ?: 0L }
        val storageFull = rows.any { (_, s) -> s is Status.Queued && s.reason == WaitReason.Space }

        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
            item { StorageMeter(onDisk, settings, Modifier.padding(horizontal = gutter, vertical = 8.dp), onClick = { navigator.tab(Screen.Tab.Settings) }) }
            if (fit.pinsOverBudget || storageFull) {
                item {
                    Column(Modifier.padding(horizontal = gutter, vertical = 8.dp)) {
                        Banner(
                            if (storageFull) "Storage full, downloads paused."
                            else "Pinned downloads exceed your ${formatBytes(settings.budget)} budget. Auto downloads are off until you remove pins or raise the budget.",
                        )
                        LinkButton("Change budget", onClick = { navigator.tab(Screen.Tab.Settings) })
                    }
                }
            }
            if (list.isEmpty()) {
                item {
                    EmptyState(PodIcons.Download, "Nothing downloaded", "Tap Download on an episode, or turn on auto-download for a show.")
                }
            }
            val sections = listOf(
                "In progress" to rows.filter { (_, s) -> s is Status.Queued || s is Status.Downloading || s is Status.Retrying },
                "Failed" to rows.filter { (_, s) -> s is Status.Failed },
                "Skipped" to rows.filter { (_, s) -> s is Status.Skipped },
                "Downloaded" to rows.filter { (_, s) -> s is Status.Downloaded },
            )
            for ((title, section) in sections) {
                if (section.isEmpty()) continue
                item(key = "heading:$title") { GroupHeading(title) }
                itemsIndexed(section, key = { _, it -> it.first.episodeId }) { index, (entry, s) ->
                    if (index > 0) RowDivider()
                    DownloadRow(entry, s, now, onOpen = { navigator.open(Screen.Episode(entry.episodeId)) }) { act(entry.episodeId, entry.podcastId, it) }
                }
            }
        }
    }
}

/** Everything on disk, partial files included, against the budget (M9 annotation 1). */
@Composable
private fun StorageMeter(onDisk: Long, settings: DownloadSettings, modifier: Modifier, onClick: () -> Unit) {
    val colors = podColors
    Panel(modifier) {
        Column(Modifier.clickable(onClick = onClick).padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("${formatBytes(onDisk)} of ${formatBytes(settings.budget)}", style = PodType.section.tabular(), color = colors.ink, modifier = Modifier.weight(1f))
                Text(if (settings.onMetered) "Mobile data allowed" else "Wi-Fi only", style = PodType.meta, color = colors.muted)
            }
            ProgressBar(progress = { (onDisk.toFloat() / settings.budget.coerceAtLeast(1)).coerceIn(0f, 1f) }, Modifier.fillMaxWidth())
        }
    }
}

@Composable
private fun DownloadRow(entry: Entry, status: Status, now: Long, onOpen: () -> Unit, onAct: (Act) -> Unit) {
    val colors = podColors
    Row(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = LocalGutter.current - 12.dp)
            .clip(RoundedCornerShape(12.dp))
            .clickable(onClick = onOpen)
            .padding(start = 12.dp, end = 4.dp, top = 12.dp, bottom = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusIcon(status)
        Spacer(Modifier.size(14.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(entry.podcastTitle, style = PodType.metaMedium, color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(entry.title, style = PodType.rowTitle, color = colors.ink, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Text(
                listOf(statusLabel(status, now), sizeLabel(entry), sourceLabel(entry)).joinToString(" · "),
                style = PodType.meta.tabular(), color = if (status is Status.Failed) colors.danger else colors.muted,
            )
            if (status is Status.Downloading && status.total != null && status.total > 0) {
                Spacer(Modifier.height(6.dp))
                ProgressBar(progress = { status.done.toFloat() / status.total }, Modifier.fillMaxWidth())
            }
        }
        actions(status).firstOrNull()?.let { PodButton(it.label, { onAct(it) }, Modifier.padding(start = 8.dp), height = 34.dp, style = PodType.metaMedium) }
    }
}

/** The legend's glyph for a status (M2 annotation 3): the download glyphs, a ring while downloading. */
@Composable
private fun StatusIcon(status: Status?) {
    val colors = podColors
    when (status) {
        null -> Unit
        is Status.Downloading -> {
            val fraction = status.total?.takeIf { it > 0 }?.let { status.done.toFloat() / it }
            if (fraction != null) {
                CircularProgressIndicator(progress = { fraction }, Modifier.size(22.dp), color = colors.accent, trackColor = colors.line, strokeWidth = 2.dp, gapSize = 0.dp)
            } else {
                CircularProgressIndicator(Modifier.size(22.dp), color = colors.accent, trackColor = colors.line, strokeWidth = 2.dp)
            }
        }
        else -> DownloadMark(status, Modifier.size(22.dp))
    }
}

/**
 * The episode screen's download button (M7 annotation 2): "Download" pins an episode nothing
 * wishes yet; otherwise it shows the status and offers that state's ways out.
 */
@Composable
fun DownloadButton(episodeId: String, podcastId: String, modifier: Modifier = Modifier, height: Dp = 40.dp) {
    val status = LocalDownloadStatuses.current[episodeId]
    val act = rememberDownloadAction()
    val now = rememberNow(1_000)
    val colors = podColors
    var menu by remember { mutableStateOf(false) }
    if (status == null) {
        PodButton("Download", { act(episodeId, podcastId, Act.Download) }, modifier, icon = PodIcons.Download, height = height)
        return
    }
    Box(modifier) {
        val label = statusLabel(status, now).substringBefore(" ·").substringBefore(" (")
        val icon = when (status) {
            is Status.Downloaded -> PodIcons.Downloaded
            is Status.Failed -> PodIcons.Alert
            is Status.Queued -> PodIcons.Queued
            is Status.Retrying -> PodIcons.Retry
            Status.Skipped -> PodIcons.Minus
            is Status.Downloading -> PodIcons.Download
        }
        PodButton(label, { menu = true }, Modifier.fillMaxWidth(), icon = icon, height = height)
        DropdownMenu(menu, onDismissRequest = { menu = false }, shape = RoundedCornerShape(12.dp), containerColor = colors.surface) {
            DropdownMenuItem(text = { Text(statusLabel(status, now), color = colors.muted) }, onClick = {}, enabled = false)
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
