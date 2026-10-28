package ltd.kilian.pod.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier

// TODO: these stand in for the playback service's controller (spec 8) and the download
// reconciler's state (spec 7) until those land; they render nothing and do nothing.

/** What the screens ask of the player. */
interface PlayerControls {
    fun play(episodeId: String)
}

@Composable
fun rememberPlayer(): PlayerControls = remember {
    object : PlayerControls {
        override fun play(episodeId: String) = Unit
    }
}

@Composable
fun MiniPlayer() = Unit

@Composable
fun DownloadGlyph(episodeId: String) = Unit

@Composable
fun DownloadButton(episodeId: String) = Unit

/** The ids of the podcast's episodes with a complete download, for the "Downloaded" filter. */
@Composable
fun rememberDownloadedEpisodes(podcastId: String): Set<String> = emptySet()

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DownloadsScreen() {
    Column(Modifier.fillMaxSize()) {
        TopAppBar(title = { Text("Downloads") })
    }
}
