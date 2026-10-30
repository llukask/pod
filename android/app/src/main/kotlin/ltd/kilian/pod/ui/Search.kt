package ltd.kilian.pod.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import ltd.kilian.pod.core.DirectoryResult
import ltd.kilian.pod.core.FeedUrl
import ltd.kilian.pod.core.FeedUrlResult
import ltd.kilian.pod.search.Query
import ltd.kilian.pod.search.SearchResult
import ltd.kilian.pod.search.classify
import ltd.kilian.pod.search.feedUrlProblem
import ltd.kilian.pod.search.isSubscribed
import ltd.kilian.pod.search.loadSubscribed
import ltd.kilian.pod.search.search
import ltd.kilian.pod.store.observe
import ltd.kilian.pod.sync.DirectoryHint
import ltd.kilian.pod.sync.subscribe

// ==============================================================================================
// Search and add (wireframe M4)
// ==============================================================================================
//
// Queries the directory as the user types, debounced 300 ms (spec 4.6). Search needs the
// network; subscribing doesn't, and pasting a feed URL skips the directory. The same subscribe
// path runs either way (spec 5.5).

private const val DEBOUNCE_MS = 300L

private sealed interface Results {
    data object Idle : Results
    data object Searching : Results
    data class Found(val results: List<DirectoryResult>) : Results
    data class Failed(val message: String) : Results
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SearchScreen() {
    val pod = LocalPod.current
    val navigator = LocalNavigator.current
    val scope = rememberCoroutineScope()
    var input by rememberSaveable { mutableStateOf("") }
    var results by remember { mutableStateOf<Results>(Results.Idle) }
    var addByUrl by rememberSaveable { mutableStateOf(false) }
    val subscribed by remember { pod.database.observe("subscription") { loadSubscribed(it) } }.collectAsState(initial = emptySet())
    val network by pod.network.state.collectAsState()
    val query = classify(input)

    LaunchedEffect(query, network.connected) {
        if (query !is Query.Terms) {
            results = Results.Idle
            return@LaunchedEffect
        }
        delay(DEBOUNCE_MS)
        results = Results.Searching
        val found = withContext(Dispatchers.IO) { search(pod.database.openHelper.readableDatabase, pod.engine.http, network.connected, query.terms) }
        results = when (found) {
            // The directory can list one show under several feed URLs that the server resolves to
            // the same canonical id; they are one show here.
            is SearchResult.Ok -> Results.Found(found.results.distinctBy { it.podcastId })
            is SearchResult.Failed -> Results.Failed(found.error.message)
        }
    }

    fun add(feedUrl: String, hint: DirectoryHint?) {
        when (val normalized = FeedUrl.normalize(feedUrl)) {
            is FeedUrlResult.Ok -> scope.launch {
                val id = subscribe(pod.database, pod.sync, normalized.url, hint)
                navigator.back()
                navigator.open(Screen.Podcast(id))
            }
            is FeedUrlResult.Err -> navigator.tell(feedUrlProblem(normalized.error))
        }
    }

    val colors = podColors
    val gutter = LocalGutter.current
    Column(Modifier.fillMaxSize().imePadding()) {
        Row(Modifier.fillMaxWidth().padding(start = gutter - 10.dp, end = gutter, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            PodIconButton(PodIcons.ArrowLeft, "Back", onClick = { navigator.back() }, tint = colors.accentInk)
            PodTextField(
                input, { input = it }, label = null,
                placeholder = "Search podcasts or paste a feed URL",
                leadingIcon = PodIcons.Search,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                modifier = Modifier.weight(1f),
            )
        }
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
            item {
                when (query) {
                    // A pasted URL subscribes straight away; one that doesn't normalize says why.
                    is Query.Feed -> when (val feed = query.result) {
                        is FeedUrlResult.Ok -> ListRow(PodIcons.Link, "Add ${FeedUrl.host(feed.url.feedUrl)}", "Subscribe to this feed") { add(feed.url.feedUrl, null) }
                        is FeedUrlResult.Err -> Text(feedUrlProblem(feed.error), style = PodType.body, color = colors.danger, modifier = Modifier.padding(horizontal = gutter, vertical = 12.dp))
                    }
                    else -> ListRow(PodIcons.Link, "Add by feed URL", if (network.connected) "Paste an RSS or Atom link" else "Works offline") { addByUrl = true }
                }
            }
            when (val shown = results) {
                Results.Idle -> if (input.isBlank()) item {
                    EmptyState(PodIcons.Search, "Find a podcast", "Search the directory by name, or paste the address of a feed.")
                }
                Results.Searching -> item {
                    LinearProgressIndicator(Modifier.fillMaxWidth().padding(horizontal = gutter, vertical = 16.dp), color = colors.accent, trackColor = colors.line)
                }
                is Results.Failed -> item { Text(shown.message, style = PodType.body, color = colors.muted, modifier = Modifier.padding(horizontal = gutter, vertical = 16.dp)) }
                is Results.Found -> {
                    item {
                        Text(
                            "Results for \u201c${(query as? Query.Terms)?.terms ?: input.trim()}\u201d",
                            style = PodType.pageTitle, color = colors.ink, maxLines = 2, overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.padding(start = gutter, end = gutter, top = 20.dp, bottom = 4.dp),
                        )
                    }
                    if (shown.results.isEmpty()) item { Text("No shows found.", style = PodType.body, color = colors.muted, modifier = Modifier.padding(horizontal = gutter, vertical = 16.dp)) }
                    itemsIndexed(shown.results, key = { _, it -> it.podcastId }) { index, result ->
                        if (index > 0) RowDivider()
                        ResultRow(result, isSubscribed(result, subscribed)) { add(result.feedUrl, DirectoryHint(result.title, result.artworkUrl)) }
                    }
                }
            }
        }
    }
    if (addByUrl) AddByUrlSheet(onDismiss = { addByUrl = false }, onAdd = { add(it, null) })
}

@Composable
private fun ListRow(icon: ImageVector, title: String, subtitle: String, onClick: () -> Unit) {
    val colors = podColors
    Row(
        Modifier.fillMaxWidth().padding(horizontal = LocalGutter.current - 12.dp).clip(RoundedCornerShape(12.dp)).clickable(onClick = onClick).padding(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(56.dp).clip(RoundedCornerShape(8.dp)).background(colors.accentSoft), contentAlignment = Alignment.Center) {
            Icon(icon, contentDescription = null, tint = colors.accentInk, modifier = Modifier.size(24.dp))
        }
        Spacer(Modifier.width(14.dp))
        Column {
            Text(title, style = PodType.rowTitle, color = colors.ink)
            Text(subtitle, style = PodType.meta, color = colors.muted)
        }
    }
}

@Composable
private fun ResultRow(result: DirectoryResult, subscribed: Boolean, onSubscribe: () -> Unit) {
    val colors = podColors
    Row(Modifier.fillMaxWidth().padding(horizontal = LocalGutter.current, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        Artwork(result.artworkUrl, 56.dp, result.title, shadow = true, radius = 8.dp)
        Spacer(Modifier.width(14.dp))
        Column(Modifier.weight(1f)) {
            Text(result.title, style = PodType.rowTitle, color = colors.ink, maxLines = 2, overflow = TextOverflow.Ellipsis)
            result.author?.let { Text(it, style = PodType.meta, color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis) }
        }
        Spacer(Modifier.width(12.dp))
        if (subscribed) {
            PodButton("Subscribed", {}, enabled = true, icon = PodIcons.Check, height = 36.dp, style = PodType.metaMedium)
        } else {
            PodButton("Subscribe", onSubscribe, kind = ButtonKind.Primary, icon = PodIcons.Plus, height = 36.dp, style = PodType.metaMedium)
        }
    }
}

/** The "Add by feed URL" sheet: checks the URL locally and shows problems under the field. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AddByUrlSheet(onDismiss: () -> Unit, onAdd: (String) -> Unit) {
    var url by rememberSaveable { mutableStateOf("") }
    val problem = when (val result = if (url.isBlank()) null else FeedUrl.normalize(url.trim())) {
        is FeedUrlResult.Err -> feedUrlProblem(result.error)
        else -> null
    }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = podColors.background, shape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp)) {
        Column(Modifier.padding(horizontal = 24.dp).padding(bottom = 24.dp).imePadding(), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Text("Add by feed URL", style = PodType.detailTitle, color = podColors.ink)
            PodTextField(
                url, { url = it }, label = "Feed URL",
                error = problem,
                supporting = "The show appears in Podcasts right away and fills in after the next sync.",
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                modifier = Modifier.fillMaxWidth(),
            )
            PodButton(
                "Subscribe",
                onClick = {
                    onDismiss()
                    onAdd(url.trim())
                },
                kind = ButtonKind.Primary,
                enabled = url.isNotBlank() && problem == null,
                height = 46.dp,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}
