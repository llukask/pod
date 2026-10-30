package ltd.kilian.pod.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.ExperimentalTextApi
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import ltd.kilian.pod.R

// ==============================================================================================
// Tokens
// ==============================================================================================
//
// The interface stays quiet so the cover art carries the colour: neutrals with a little green in
// them, and one jade accent used only for state (what's playing, how far, what's selected, the
// primary button). Each show's own cover, blurred, washes the surfaces that belong to it
// ([Ambient]). The values are the web client's (`web/src/styles.css`), so the apps match.
//
// The palette follows the system's light or dark mode and ignores the wallpaper: the accent is
// the brand, and a wallpaper-derived one would fight the covers.

@Immutable
data class PodColors(
    val background: Color,
    val surface: Color,
    /** The tab bar and the wide layout's sidebar. */
    val sidebar: Color,
    val ink: Color,
    val muted: Color,
    val faint: Color,
    val line: Color,
    /** A pressed or hovered row: ink at 5 or 6 %. */
    val hover: Color,
    val accent: Color,
    /** Accent-coloured text, darker (or lighter in dark mode) than [accent] to stay readable. */
    val accentInk: Color,
    /** A selected row, a placeholder cover. */
    val accentSoft: Color,
    val onAccent: Color,
    val danger: Color,
    /** "Waiting" beside the accent's "done", e.g. changes not yet on the server. */
    val warning: Color,
    /** How strongly a blurred cover washes a surface ([Ambient]). */
    val ambient: Float,
    val dark: Boolean,
)

val LightPodColors = PodColors(
    background = Color(0xFFF4F5F1),
    surface = Color(0xFFFFFFFF),
    sidebar = Color(0xFFEAECE6),
    ink = Color(0xFF17201B),
    muted = Color(0xFF58625D),
    faint = Color(0xFF89918C),
    line = Color(0xFFDADFD7),
    hover = Color(0xFF17201B).copy(alpha = 0.05f),
    accent = Color(0xFF0A7552),
    accentInk = Color(0xFF076142),
    accentSoft = Color(0xFFD9EEE3),
    onAccent = Color(0xFFFFFFFF),
    danger = Color(0xFFB42318),
    warning = Color(0xFFD69E2E),
    ambient = 0.30f,
    dark = false,
)

val DarkPodColors = PodColors(
    background = Color(0xFF0F1311),
    surface = Color(0xFF161B18),
    sidebar = Color(0xFF0B0E0C),
    ink = Color(0xFFE6EBE8),
    muted = Color(0xFF9BA5A0),
    faint = Color(0xFF6C7570),
    line = Color(0xFF262D29),
    hover = Color(0xFFE6EBE8).copy(alpha = 0.06f),
    accent = Color(0xFF4FCF9B),
    accentInk = Color(0xFF79DFB6),
    accentSoft = Color(0xFF163128),
    onAccent = Color(0xFF052419),
    danger = Color(0xFFFF8A7A),
    warning = Color(0xFFD69E2E),
    ambient = 0.38f,
    dark = true,
)

val LocalPodColors = staticCompositionLocalOf { LightPodColors }

// ==============================================================================================
// Type
// ==============================================================================================
//
// Radio Canada, a face drawn for a broadcaster's screens, for the interface and reading, and
// Radio Canada Big for headings. Both are variable fonts (OFL, from github.com/google/fonts);
// each weight below is an instance of the one file, picked through its `wght` axis. The narrow
// width (`wdth` 85) is for the player's clocks, where digits line up in less room.

@OptIn(ExperimentalTextApi::class)
private fun radioCanada(weight: Int, width: Float = 100f) = Font(
    R.font.radio_canada,
    weight = FontWeight(weight),
    variationSettings = FontVariation.Settings(FontVariation.weight(weight), FontVariation.width(width)),
)

@OptIn(ExperimentalTextApi::class)
private fun radioCanadaBig(weight: Int) = Font(
    R.font.radio_canada_big,
    weight = FontWeight(weight),
    variationSettings = FontVariation.Settings(FontVariation.weight(weight)),
)

val RadioCanada = FontFamily(listOf(300, 400, 450, 500, 600, 650, 700).map { radioCanada(it) })
val RadioCanadaNarrow = FontFamily(listOf(400, 500, 600).map { radioCanada(it, width = 85f) })
val RadioCanadaBig = FontFamily(listOf(500, 600, 650, 700).map { radioCanadaBig(it) })

/** Line heights sit centred on the text, so a single line in a pill or a row centres optically. */
private val Centered = LineHeightStyle(LineHeightStyle.Alignment.Center, LineHeightStyle.Trim.None)

private fun ui(size: TextUnit, weight: Int = 400, lineHeight: TextUnit = size * 1.4, tracking: TextUnit = 0.sp) = TextStyle(
    fontFamily = RadioCanada,
    fontWeight = FontWeight(weight),
    fontSize = size,
    lineHeight = lineHeight,
    letterSpacing = tracking,
    lineHeightStyle = Centered,
)

private fun display(size: TextUnit, weight: Int = 700, tracking: Float = -0.025f, lineHeight: Float = 1.12f) = TextStyle(
    fontFamily = RadioCanadaBig,
    fontWeight = FontWeight(weight),
    fontSize = size,
    lineHeight = size * lineHeight,
    letterSpacing = tracking.em,
    lineHeightStyle = Centered,
)

/** The type scale (13, 15, 17, 22, 28/30): sentence case everywhere, no all-caps labels. */
object PodType {
    /** A tab's or page's title: "Inbox", "Settings". */
    val pageTitle = display(28.sp)
    /** A show's name on its page, the sign-in card's title. */
    val showTitle = display(26.sp, lineHeight = 1.1f)
    /** An episode's title on its page, the player sheet's title, dialog and empty-state titles. */
    val detailTitle = display(22.sp, tracking = -0.015f, lineHeight = 1.2f)
    /** A settings section, a sheet's heading. */
    val section = ui(17.sp, 650, tracking = (-0.01).em)
    val body = ui(15.sp)
    val bodyMedium = ui(15.sp, 500)
    /** An episode's title in a list. */
    val rowTitle = ui(15.sp, 600, lineHeight = 19.5.sp)
    /** Show notes: a little larger, with room between lines for reading. */
    val reading = ui(16.sp, lineHeight = 26.sp)
    val button = ui(15.sp, 500)
    val meta = ui(13.sp, lineHeight = 18.sp)
    val metaMedium = ui(13.sp, 500, lineHeight = 18.sp)
    /** Group headings ("Today"), the speed pill. */
    val label = ui(13.sp, 650, lineHeight = 18.sp)
    val tab = ui(11.sp, 600, lineHeight = 14.sp)
    /** Elapsed and remaining time: narrow, tabular digits. */
    val clock = TextStyle(
        fontFamily = RadioCanadaNarrow,
        fontWeight = FontWeight(500),
        fontSize = 13.sp,
        fontFeatureSettings = "tnum",
        lineHeightStyle = Centered,
    )
}

/** Tabular numerals, for durations that sit in columns or tick. */
fun TextStyle.tabular() = copy(fontFeatureSettings = "tnum")

// ==============================================================================================
// The theme
// ==============================================================================================

/**
 * The Pod tokens as a [CompositionLocal][LocalPodColors] plus a Material scheme built from them,
 * so the Material pieces still in use (menus, switches, sheets, pull to refresh) wear the same
 * colours and type.
 */
@Composable
fun PodTheme(content: @Composable () -> Unit) {
    val colors = if (isSystemInDarkTheme()) DarkPodColors else LightPodColors
    val scheme = if (colors.dark) {
        darkColorScheme(
            primary = colors.accent, onPrimary = colors.onAccent,
            primaryContainer = colors.accentSoft, onPrimaryContainer = colors.accentInk,
            secondary = colors.accent, onSecondary = colors.onAccent,
            secondaryContainer = colors.accentSoft, onSecondaryContainer = colors.accentInk,
            tertiary = colors.accent, onTertiary = colors.onAccent,
            background = colors.background, onBackground = colors.ink,
            surface = colors.surface, onSurface = colors.ink,
            surfaceVariant = colors.sidebar, onSurfaceVariant = colors.muted,
            surfaceContainerLowest = colors.surface, surfaceContainerLow = colors.surface,
            surfaceContainer = colors.surface, surfaceContainerHigh = colors.surface, surfaceContainerHighest = colors.line,
            surfaceTint = Color.Transparent,
            outline = colors.faint, outlineVariant = colors.line,
            error = colors.danger, onError = colors.background,
            inverseSurface = colors.ink, inverseOnSurface = colors.background, inversePrimary = LightPodColors.accent,
            scrim = Color(0xFF080C0A),
        )
    } else {
        lightColorScheme(
            primary = colors.accent, onPrimary = colors.onAccent,
            primaryContainer = colors.accentSoft, onPrimaryContainer = colors.accentInk,
            secondary = colors.accent, onSecondary = colors.onAccent,
            secondaryContainer = colors.accentSoft, onSecondaryContainer = colors.accentInk,
            tertiary = colors.accent, onTertiary = colors.onAccent,
            background = colors.background, onBackground = colors.ink,
            surface = colors.surface, onSurface = colors.ink,
            surfaceVariant = colors.sidebar, onSurfaceVariant = colors.muted,
            surfaceContainerLowest = colors.surface, surfaceContainerLow = colors.surface,
            surfaceContainer = colors.surface, surfaceContainerHigh = colors.surface, surfaceContainerHighest = colors.line,
            surfaceTint = Color.Transparent,
            outline = colors.faint, outlineVariant = colors.line,
            error = colors.danger, onError = colors.surface,
            inverseSurface = colors.ink, inverseOnSurface = colors.background, inversePrimary = DarkPodColors.accent,
            scrim = Color(0xFF080C0A),
        )
    }
    val base = Typography()
    val typography = Typography(
        displayLarge = base.displayLarge.copy(fontFamily = RadioCanadaBig),
        displayMedium = base.displayMedium.copy(fontFamily = RadioCanadaBig),
        displaySmall = base.displaySmall.copy(fontFamily = RadioCanadaBig),
        headlineLarge = base.headlineLarge.copy(fontFamily = RadioCanadaBig),
        headlineMedium = base.headlineMedium.copy(fontFamily = RadioCanadaBig),
        headlineSmall = PodType.detailTitle,
        titleLarge = PodType.detailTitle,
        titleMedium = PodType.section,
        titleSmall = PodType.rowTitle,
        bodyLarge = PodType.reading.copy(lineHeight = 22.sp),
        bodyMedium = PodType.body,
        bodySmall = PodType.meta,
        labelLarge = PodType.button,
        labelMedium = PodType.metaMedium,
        labelSmall = PodType.tab,
    )
    CompositionLocalProvider(LocalPodColors provides colors) {
        MaterialTheme(colorScheme = scheme, typography = typography, content = content)
    }
}

/** The current tokens. */
val podColors: PodColors
    @Composable @ReadOnlyComposable get() = LocalPodColors.current
