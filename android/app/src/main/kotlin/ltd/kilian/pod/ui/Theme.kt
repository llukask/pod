package ltd.kilian.pod.ui

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

// A warm amber accent on neutral surfaces, so artwork carries the colour. Android 12+ follows the
// wallpaper instead (minSdk 34 means that is every device), and these are the fallback for
// previews and tests.
private val Amber = Color(0xFFB4690E)
private val AmberLight = Color(0xFFFFB86B)

@Composable
fun PodTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val context = LocalContext.current
    val colors = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && dark -> dynamicDarkColorScheme(context)
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> dynamicLightColorScheme(context)
        dark -> darkColorScheme(primary = AmberLight)
        else -> lightColorScheme(primary = Amber)
    }
    MaterialTheme(colorScheme = colors, content = content)
}
