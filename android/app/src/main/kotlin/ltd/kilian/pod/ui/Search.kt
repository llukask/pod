package ltd.kilian.pod.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.TopAppBar
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
import androidx.compose.ui.graphics.Color
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

    Column(Modifier.fillMaxSize().imePadding()) {
        TopAppBar(
            navigationIcon = { IconButton(onClick = { navigator.back() }) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") } },
            title = {
                TextField(
                    input, { input = it }, placeholder = { Text("Search podcasts") }, singleLine = true,
                    colors = TextFieldDefaults.colors(focusedContainerColor = Color.Transparent, unfocusedContainerColor = Color.Transparent),
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                    modifier = Modifier.fillMaxWidth(),
                )
            },
        )
        LazyColumn(Modifier.fillMaxSize()) {
            item {
                when (query) {
                    // A pasted URL subscribes straight away; one that doesn't normalize says why.
                    is Query.Feed -> when (val feed = query.result) {
                        is FeedUrlResult.Ok -> ListRow(Icons.Outlined.Link, "Add ${FeedUrl.host(feed.url.feedUrl)}", "Subscribe to this feed") { add(feed.url.feedUrl, null) }
                        is FeedUrlResult.Err -> Text(feedUrlProblem(feed.error), color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(16.dp))
                    }
                    else -> ListRow(Icons.Outlined.Link, "Add by feed URL", if (network.connected) "Paste an RSS or Atom link" else "Works offline") { addByUrl = true }
                }
                HorizontalDivider()
            }
            when (val shown = results) {
                Results.Idle -> Unit
                Results.Searching -> item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
                is Results.Failed -> item { Text(shown.message, Modifier.padding(16.dp)) }
                is Results.Found -> {
                    if (shown.results.isEmpty()) item { Text("No shows found.", Modifier.padding(16.dp)) }
                    items(shown.results, key = { it.podcastId }) { result ->
                        ResultRow(result, isSubscribed(result, subscribed)) { add(result.feedUrl, DirectoryHint(result.title, result.artworkUrl)) }
                    }
                }
            }
        }
    }
    if (addByUrl) AddByUrlSheet(onDismiss = { addByUrl = false }, onAdd = { add(it, null) })
}

@Composable
private fun ListRow(icon: androidx.compose.ui.graphics.vector.ImageVector, title: String, subtitle: String, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().clickable(onClick = onClick).padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, contentDescription = null)
        Spacer(Modifier.width(16.dp))
        Column {
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(subtitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun ResultRow(result: DirectoryResult, subscribed: Boolean, onSubscribe: () -> Unit) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Artwork(result.artworkUrl, 56.dp)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(result.title, style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            result.author?.let { Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis) }
        }
        if (subscribed) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.Check, contentDescription = null)
                Text("Subscribed", style = MaterialTheme.typography.labelLarge)
            }
        } else {
            FilledTonalButton(onClick = onSubscribe) { Text("Subscribe") }
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
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 24.dp).padding(bottom = 24.dp).imePadding(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Add by feed URL", style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(
                url, { url = it }, label = { Text("Feed URL") }, singleLine = true, isError = problem != null,
                supportingText = { Text(problem ?: "The show appears in Podcasts right away and fills in after the next sync.") },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                modifier = Modifier.fillMaxWidth(),
            )
            Button(
                onClick = {
                    onDismiss()
                    onAdd(url.trim())
                },
                enabled = url.isNotBlank() && problem == null,
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Subscribe") }
        }
    }
}
