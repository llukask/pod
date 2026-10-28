package ltd.kilian.pod.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember

// TODO: these stand in for the playback service's controller (spec 8) until it lands; they
// render nothing and do nothing.

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

