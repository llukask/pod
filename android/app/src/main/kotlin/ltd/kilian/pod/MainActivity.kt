package ltd.kilian.pod

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import ltd.kilian.pod.ui.PodRoot
import ltd.kilian.pod.ui.PodTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent { PodTheme { PodRoot() } }
    }
}
