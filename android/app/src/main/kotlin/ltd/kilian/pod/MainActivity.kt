package ltd.kilian.pod

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import ltd.kilian.pod.player.PlayerConnection
import ltd.kilian.pod.ui.LocalPlayer
import ltd.kilian.pod.ui.PodRoot
import ltd.kilian.pod.ui.PodTheme

class MainActivity : ComponentActivity() {
    /** Connecting starts the playback service, which restores the last episode for the mini player. */
    private lateinit var player: PlayerConnection

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        player = PlayerConnection(this)
        setContent { PodTheme { CompositionLocalProvider(LocalPlayer provides player) { PodRoot() } } }
    }

    override fun onDestroy() {
        player.close()
        super.onDestroy()
    }
}
