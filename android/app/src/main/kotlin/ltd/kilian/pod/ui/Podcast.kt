package ltd.kilian.pod.ui

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.fromHtml
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.ZoneId
import kotlinx.coroutines.launch
import ltd.kilian.pod.inbox.dayLabel
import ltd.kilian.pod.podcast.AUTO_KEEP_MAX
import ltd.kilian.pod.podcast.DeviceSettings
import ltd.kilian.pod.podcast.PODCAST_TABLES
import ltd.kilian.pod.podcast.PodcastPage
import ltd.kilian.pod.podcast.PodcastState
import ltd.kilian.pod.podcast.autoLabel
import ltd.kilian.pod.podcast.loadPage
import ltd.kilian.pod.podcast.markAllPlayed
import ltd.kilian.pod.podcast.markAllPlayedTitle
import ltd.kilian.pod.podcast.setDeviceSettings
import ltd.kilian.pod.podcast.statusNotice
import ltd.kilian.pod.podcast.unsubscribe
import ltd.kilian.pod.podcast.unsubscribeMessage
import ltd.kilian.pod.store.observe

// ==============================================================================================
// Podcast (wireframes M5 and M6)
// ==============================================================================================
//
// The show's whole history from the local mirror, newest first. Its cover, blurred, washes the
// top of the page and fades into it. The header holds the Subscribed pill, the gear for this
// device's settings for the show, and a menu with "Mark all played" and Unsubscribe, which
// confirms with what goes (M11).
//
// A pane 1000 dp wide or more (a tablet in landscape) turns the header into a 320 dp side column
// beside the episodes; the list itself shows dates and lengths as columns from 720 dp.

private enum class Filter(val label: String) { All("All"), Unplayed("Unplayed"), Downloaded("Downloaded") }

/** How far down the wash reaches before it has faded out. */
private val WASH_HEIGHT = 480.dp

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun PodcastScreen(id: String) {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val scope = rememberCoroutineScope()
    val now = rememberNow()
    val zone = remember { ZoneId.systemDefault() }
    // A page that was there and then isn't means the show left (unsubscribed here or elsewhere,
    // or replaced by its canonical id); the screen goes back rather than showing nothing.
    val page by remember(id) { pod.database.observe(*PODCAST_TABLES) { Loaded(loadPage(it, id)) } }.collectAsState(initial = null)
    var filter by rememberSaveable { mutableStateOf(Filter.All) }
    var settingsSheet by rememberSaveable { mutableStateOf(false) }
    var confirmUnsubscribe by rememberSaveable { mutableStateOf(false) }
    var confirmMarkAll by rememberSaveable { mutableStateOf(false) }

    val loaded = page ?: return
    val show = loaded.page ?: run {
        navigator.back()
        return
    }
    val downloaded = rememberDownloadedEpisodes()
    val episodes = when (filter) {
        Filter.All -> show.episodes
        Filter.Unplayed -> show.episodes.filterNot { it.played }
        Filter.Downloaded -> show.episodes.filter { it.id in downloaded }
    }
    val unplayed = show.episodes.count { !it.played }
    val actions = HeaderActions(
        onSettings = { settingsSheet = true },
        onMarkAll = { confirmMarkAll = true }.takeIf { unplayed > 0 },
        onUnsubscribe = { confirmUnsubscribe = true },
    )
    val back = when (val previous = navigator.previous) {
        is Screen.Tab -> previous.label
        Screen.Search -> "Search"
        is Screen.Episode -> "Episode"
        is Screen.Podcast, null -> null
    }

    BoxWithConstraints(Modifier.fillMaxSize()) {
        val side = maxWidth >= 1000.dp
        val list = rememberLazyListState()
        // The wash scrolls away with the header; past the first item it has faded out anyway.
        val washOffset by remember { derivedStateOf { if (list.firstVisibleItemIndex == 0) -list.firstVisibleItemScrollOffset.toFloat() else Float.NEGATIVE_INFINITY } }
        Wash(show.artworkUrl, Modifier.graphicsLayer { translationY = if (side) 0f else washOffset.coerceAtLeast(-WASH_HEIGHT.toPx()) })

        val rows: androidx.compose.foundation.lazy.LazyListScope.(columns: Boolean) -> Unit = { columns ->
            if (show.loading) {
                item(key = "loading") {
                    Column(Modifier.padding(horizontal = LocalGutter.current, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text("Loading episodes… ${show.episodes.size} so far", style = PodType.meta, color = podColors.muted)
                        LinearProgressIndicator(Modifier.fillMaxWidth(), color = podColors.accent, trackColor = podColors.line)
                    }
                }
            }
            if (episodes.isEmpty() && !show.loading) {
                item(key = "empty") {
                    val message = when {
                        show.state == PodcastState.Pending -> "Episodes show up once the server has fetched the feed."
                        filter == Filter.Unplayed && show.episodes.isNotEmpty() -> "You've played every episode."
                        filter == Filter.Downloaded -> "No episodes of this show are downloaded."
                        else -> "This feed has no episodes."
                    }
                    Text(message, style = PodType.body, color = podColors.muted, textAlign = androidx.compose.ui.text.style.TextAlign.Center, modifier = Modifier.fillMaxWidth().padding(vertical = 72.dp, horizontal = 24.dp))
                }
            }
            itemsIndexed(episodes, key = { _, it -> it.id }) { index, episode ->
                if (index > 0) RowDivider()
                EpisodeRow(
                    RowEpisode(episode.id, show.id, null, episode.title, null, episode.durationMs, episode.positionMs, episode.played, episode.archived),
                    day = dayLabel(episode.publishedAt, now, zone),
                    showPodcast = false,
                    columns = columns,
                    onClick = { navigator.open(Screen.Episode(episode.id)) },
                )
            }
        }

        if (side) {
            Row(Modifier.fillMaxSize().statusBarsPadding()) {
                Column(Modifier.width(320.dp + 28.dp).fillMaxHeight().verticalScroll(rememberScrollState()).padding(start = 28.dp, end = 4.dp, bottom = 24.dp)) {
                    BackBar(back, onBack = { navigator.back() }, Modifier.padding(start = 0.dp))
                    SideHeader(show, now, actions)
                }
                BoxWithConstraints(Modifier.weight(1f).fillMaxHeight()) {
                    val columns = maxWidth >= 720.dp
                    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(top = 52.dp, bottom = 24.dp)) {
                        item(key = "filters") { Filters(filter, downloaded.isNotEmpty()) { filter = it } }
                        rows(columns)
                    }
                }
            }
        } else {
            val columns = maxWidth >= 720.dp
            val artSize = if (maxWidth >= 600.dp) 160.dp else 112.dp
            LazyColumn(Modifier.fillMaxSize(), state = list, contentPadding = PaddingValues(bottom = 24.dp)) {
                item(key = "header") {
                    Column(Modifier.statusBarsPadding()) {
                        BackBar(back, onBack = { navigator.back() })
                        Header(show, now, actions, artSize)
                    }
                }
                item(key = "filters") { Filters(filter, downloaded.isNotEmpty()) { filter = it } }
                rows(columns)
            }
        }
    }

    if (settingsSheet) {
        PodcastSettingsSheet(show, onDismiss = { settingsSheet = false }, onUnsubscribe = {
            settingsSheet = false
            confirmUnsubscribe = true
        })
    }
    if (confirmUnsubscribe) {
        PodDialog(
            title = "Unsubscribe from ${show.title}?",
            message = unsubscribeMessage(show.downloads),
            confirm = "Unsubscribe",
            onConfirm = {
                confirmUnsubscribe = false
                scope.launch { unsubscribe(pod.database, pod.sync, show.id, show.feedUrl) }
            },
            onDismiss = { confirmUnsubscribe = false },
        )
    }
    if (confirmMarkAll) {
        PodDialog(
            title = markAllPlayedTitle(unplayed),
            message = "They leave the inbox, and their downloads are removed from this phone.",
            confirm = "Mark played",
            onConfirm = {
                confirmMarkAll = false
                scope.launch { markAllPlayed(pod.database, pod.sync, show.id) }
            },
            onDismiss = { confirmMarkAll = false },
        )
    }
}

/** The page query's result, so "not subscribed" and "not read yet" differ. */
private data class Loaded(val page: PodcastPage?)

/** What the header's buttons do; [onMarkAll] is null when nothing is left unplayed. */
private class HeaderActions(val onSettings: () -> Unit, val onMarkAll: (() -> Unit)?, val onUnsubscribe: () -> Unit)

/**
 * The show's cover blown up behind the top of the page, fading into the page colour by
 * [WASH_HEIGHT]. The fade is the page colour drawn over the wash, rather than a mask, since the
 * page behind is one flat colour anyway.
 */
@Composable
private fun Wash(url: String?, modifier: Modifier) {
    val background = podColors.background
    Box(
        modifier
            .fillMaxWidth()
            .height(WASH_HEIGHT)
            .drawWithContent {
                drawContent()
                drawRect(Brush.verticalGradient(0.3f to background.copy(alpha = 0f), 1f to background))
            },
    ) {
        Ambient(url)
    }
}

@Composable
private fun Header(show: PodcastPage, now: Long, actions: HeaderActions, artSize: Dp) {
    val gutter = LocalGutter.current
    Column(Modifier.padding(start = gutter, end = gutter, top = 4.dp, bottom = 8.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(16.dp), verticalAlignment = Alignment.Bottom) {
            Artwork(show.artworkUrl, artSize, show.title, shadow = true)
            Column(Modifier.weight(1f)) {
                Title(show, PodType.showTitle.copy(fontSize = if (artSize > 120.dp) 30.sp else 24.sp, lineHeight = if (artSize > 120.dp) 33.sp else 27.sp))
                Spacer(Modifier.height(10.dp))
                ActionRow(show, actions)
            }
        }
        About(show, now, maxLines = 2, small = false)
    }
}

/** The wide layout's column: the cover at full width, then the text under it. */
@Composable
private fun SideHeader(show: PodcastPage, now: Long, actions: HeaderActions) {
    Column(Modifier.padding(top = 8.dp, end = 12.dp), verticalArrangement = Arrangement.spacedBy(0.dp)) {
        Artwork(show.artworkUrl, 320.dp, show.title, Modifier.fillMaxWidth().aspectRatio(1f), shadow = true)
        Spacer(Modifier.height(16.dp))
        Title(show, PodType.detailTitle)
        Spacer(Modifier.height(10.dp))
        ActionRow(show, actions)
        About(show, now, maxLines = 8, small = true)
    }
}

@Composable
private fun Title(show: PodcastPage, style: androidx.compose.ui.text.TextStyle) {
    Text(show.title, style = style, color = podColors.ink, maxLines = 4, overflow = TextOverflow.Ellipsis)
    show.author?.let { Text(it, style = PodType.bodyMedium, color = podColors.muted, modifier = Modifier.padding(top = 4.dp), maxLines = 2, overflow = TextOverflow.Ellipsis) }
}

@Composable
private fun ActionRow(show: PodcastPage, actions: HeaderActions) {
    var menu by remember { mutableStateOf(false) }
    Row(verticalAlignment = Alignment.CenterVertically) {
        StatePill("Subscribed", PodIcons.Check)
        PodIconButton(PodIcons.Settings, "Podcast settings", onClick = actions.onSettings, size = 40.dp, iconSize = 20.dp)
        Box {
            PodIconButton(PodIcons.More, "More", onClick = { menu = true }, size = 40.dp, iconSize = 20.dp)
            DropdownMenu(menu, onDismissRequest = { menu = false }, shape = RoundedCornerShape(12.dp), containerColor = podColors.surface) {
                DropdownMenuItem(text = { Text("Mark all played") }, enabled = actions.onMarkAll != null, onClick = {
                    menu = false
                    actions.onMarkAll?.invoke()
                })
                DropdownMenuItem(text = { Text("Unsubscribe") }, onClick = {
                    menu = false
                    actions.onUnsubscribe()
                })
            }
        }
    }
    autoLabel(show.device)?.let { label -> StatePill(label, PodIcons.Download, Modifier.padding(top = 8.dp), onClick = actions.onSettings) }
}

/** The description, clamped, with "More about this show" only when it overflows; then any notice. */
@Composable
private fun About(show: PodcastPage, now: Long, maxLines: Int, small: Boolean) {
    val colors = podColors
    var expanded by rememberSaveable(show.id) { mutableStateOf(false) }
    var overflows by remember(show.id) { mutableStateOf(false) }
    show.description?.let {
        Spacer(Modifier.height(16.dp))
        // Feeds put HTML in the description too; the header shows its text.
        Text(
            remember(it) { AnnotatedString(AnnotatedString.fromHtml(it).text.trim()) },
            style = if (small) PodType.meta.copy(lineHeight = 20.sp) else PodType.body.copy(lineHeight = 22.sp),
            color = androidx.compose.ui.graphics.lerp(colors.ink, colors.background, 0.2f),
            maxLines = if (expanded) Int.MAX_VALUE else maxLines,
            overflow = TextOverflow.Ellipsis,
            onTextLayout = { layout -> if (!expanded) overflows = layout.hasVisualOverflow },
        )
        if (overflows && !expanded) LinkButton("More about this show", onClick = { expanded = true })
    }
    if (show.state == PodcastState.Pending) {
        Spacer(Modifier.height(16.dp))
        Banner("Waiting for the server to fetch this feed.", tone = colors.accent)
    }
    statusNotice(show.state, show.episodes.size, show.downloads.total, now)?.let { notice ->
        Spacer(Modifier.height(16.dp))
        Banner(notice)
    }
}

@Composable
private fun Filters(filter: Filter, anyDownloaded: Boolean, onSelect: (Filter) -> Unit) {
    // "Downloaded" only once something is, so a show without downloads keeps the web's two choices.
    val options = if (anyDownloaded || filter == Filter.Downloaded) Filter.entries else listOf(Filter.All, Filter.Unplayed)
    Segmented(options, filter, { it.label }, onSelect, Modifier.padding(horizontal = LocalGutter.current, vertical = 8.dp))
}

/**
 * This device's settings for the show (wireframe M6), a bottom sheet. Everything here is
 * device-only (`podcast_device`, spec 7.1) and says so.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PodcastSettingsSheet(show: PodcastPage, onDismiss: () -> Unit, onUnsubscribe: () -> Unit) {
    val pod = LocalPod.current
    val scope = rememberCoroutineScope()
    val colors = podColors
    fun save(settings: DeviceSettings) = scope.launch { setDeviceSettings(pod.database, show.id, settings) }
    val device = show.device
    // Android 13+ asks before an app may notify. Turning notifications on for a show is the
    // moment the user wants them, so the prompt comes then; a refusal leaves the switch on, and
    // the worker skips posting until the permission is granted in system settings.
    val askToNotify = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = colors.background, shape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp)) {
        Column(Modifier.padding(horizontal = 16.dp).padding(bottom = 24.dp)) {
            Text(show.title, style = PodType.detailTitle, color = colors.ink)
            Text("These settings stay on this device.", style = PodType.meta, color = colors.muted, modifier = Modifier.padding(top = 4.dp, bottom = 16.dp))
            Panel {
                PanelRow("New episode notifications", note = "Only for episodes from the last 48 hours", first = true) {
                    Switch(device.notify, onCheckedChange = {
                        save(device.copy(notify = it))
                        if (it) askToNotify.launch(Manifest.permission.POST_NOTIFICATIONS)
                    }, colors = podSwitchColors())
                }
                PanelRow("Auto-download", note = if (device.autoKeep == 0) "Off" else "Keep the newest ${device.autoKeep} unplayed episodes") {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        PodIconButton(PodIcons.Minus, "Fewer", onClick = { save(device.copy(autoKeep = device.autoKeep - 1)) }, enabled = device.autoKeep > 0, size = 40.dp, iconSize = 18.dp, tint = colors.ink)
                        Text("${device.autoKeep}", style = PodType.section.tabular(), color = colors.ink, modifier = Modifier.width(24.dp), textAlign = androidx.compose.ui.text.style.TextAlign.Center)
                        PodIconButton(PodIcons.Plus, "More", onClick = { save(device.copy(autoKeep = device.autoKeep + 1)) }, enabled = device.autoKeep < AUTO_KEEP_MAX, size = 40.dp, iconSize = 18.dp, tint = colors.ink)
                    }
                }
                PanelRow("Auto-download on mobile data", note = "Otherwise waits for Wi-Fi") {
                    Switch(device.autoOnMetered, onCheckedChange = { save(device.copy(autoOnMetered = it)) }, enabled = device.autoKeep > 0, colors = podSwitchColors())
                }
            }
            Spacer(Modifier.height(16.dp))
            Panel {
                PanelRow("Unsubscribe", first = true, titleColor = colors.danger, onClick = onUnsubscribe)
            }
        }
    }
}
