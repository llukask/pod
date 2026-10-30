package ltd.kilian.pod.ui

import android.graphics.Bitmap
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.ColorMatrix
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.graphics.drawable.toBitmap
import coil.compose.AsyncImage
import coil.imageLoader
import coil.request.ImageRequest
import coil.request.SuccessResult
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

// ==============================================================================================
// Cover art
// ==============================================================================================

/** Corner radius by size: 6 at row size, 8 to 12 for larger covers, as on the web. */
private fun radiusFor(size: Dp): Dp = when {
    size < 40.dp -> 5.dp
    size < 64.dp -> 6.dp
    size < 100.dp -> 8.dp
    size < 140.dp -> 10.dp
    else -> 12.dp
}

/**
 * A podcast or episode cover, cached on disk by Coil (spec 7.5). A show without art gets a tile
 * with its initial in Radio Canada Big, so rows and grids keep their rhythm and the show stays
 * recognisable; one with no title yet (the feed's host stands in) gets a sound wave instead.
 * [shadow] lifts a large cover off the page. [size] sets the radius and the initial's size; a
 * [modifier] with its own size (a grid tile filling its column) wins over it.
 */
@Composable
fun Artwork(
    url: String?,
    size: Dp,
    title: String?,
    modifier: Modifier = Modifier,
    shadow: Boolean = false,
    radius: Dp = radiusFor(size),
) {
    val colors = podColors
    val shape = RoundedCornerShape(radius)
    val lifted = if (shadow) modifier.size(size).shadow(if (size >= 140.dp) 14.dp else 8.dp, shape, ambientColor = Color.Black, spotColor = Color.Black.copy(alpha = 0.6f)) else modifier.size(size)
    BoxWithConstraints(
        lifted.clip(shape).background(if (url == null) colors.accentSoft else colors.line),
        contentAlignment = Alignment.Center,
    ) {
        val side = minOf(maxWidth, maxHeight)
        if (url == null) {
            val letter = initial(title)
            if (letter == null) {
                Icon(PodIcons.Waves, contentDescription = null, tint = colors.accentInk, modifier = Modifier.size(side * 0.4f))
            } else {
                val fontSize = with(LocalDensity.current) { (side * 0.42f).toSp() }
                Text(letter, style = PodType.pageTitle.copy(fontSize = fontSize, lineHeight = fontSize, letterSpacing = 0.sp, fontWeight = FontWeight.Bold), color = colors.accentInk)
            }
        } else {
            AsyncImage(url, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
            // A hairline keeps a pale cover from melting into a pale page.
            Box(Modifier.fillMaxSize().border(1.dp, Color.Black.copy(alpha = 0.06f), shape))
        }
    }
}

/**
 * The letter a placeholder shows: the title's first letter, skipping a leading "The", which
 * would give half the shelf the same letter. Null when the title is the feed's host (a show the
 * server hasn't fetched yet), which starts with a digit or has no letters to speak of.
 */
fun initial(title: String?): String? {
    val words = (title ?: return null).trim().replace(Regex("^the\\s+", RegexOption.IGNORE_CASE), "")
    if (Regex("^[\\w.-]+\\.[a-z]{2,}(:\\d+)?$", RegexOption.IGNORE_CASE).matches(words)) return null
    val first = words.firstOrNull() ?: return null
    return if (first.isLetter()) first.uppercase() else null
}

// ==============================================================================================
// The ambient wash
// ==============================================================================================
//
// A show's cover blown up and blurred behind the surfaces that belong to it: the player, the
// mini player, the full player sheet, the top of a show's page. The blur is done the cheap way:
// Coil decodes the cover at 24 px, a box blur softens that tiny bitmap, and drawing it scaled up
// with bilinear filtering spreads it into a smooth wash. That costs nothing per frame, needs no
// RenderEffect, and looks the same on every API level and in screenshot tests.

private const val WASH_PX = 24

/**
 * The wash for [url] filling its parent, at the theme's ambient strength plus [extra]. The web
 * draws the image at 160 % by 220 % of the surface, offset up and left, so the colours spread
 * past the edges instead of ending in a visible tile; this does the same.
 */
@Composable
fun Ambient(url: String?, modifier: Modifier = Modifier, extra: Float = 0f) {
    val wash = rememberWash(url) ?: return
    val colors = podColors
    val alpha = (colors.ambient + extra).coerceIn(0f, 1f)
    // saturate(1.5), as the web's filter. In dark mode a pale cover is also dimmed to at most
    // 45 % lightness, or it would wash the surface grey and take the contrast from light text.
    val dim = if (colors.dark && wash.lightness > 0.45f) 0.45f / wash.lightness else 1f
    val filter = remember(dim) {
        val matrix = ColorMatrix().apply { setToSaturation(1.5f) }
        matrix *= ColorMatrix().apply { setToScale(dim, dim, dim, 1f) }
        ColorFilter.colorMatrix(matrix)
    }
    Canvas(modifier.fillMaxSize().clipToBounds()) {
        val width = size.width * 1.6f
        val height = size.height * 2.2f
        drawImage(
            wash.image,
            srcOffset = IntOffset.Zero,
            srcSize = IntSize(wash.image.width, wash.image.height),
            dstOffset = IntOffset((-size.width * 0.3f).roundToInt(), (-size.height * 0.6f).roundToInt()),
            dstSize = IntSize(width.roundToInt(), height.roundToInt()),
            alpha = alpha,
            colorFilter = filter,
            filterQuality = FilterQuality.High,
        )
    }
}

/** A blurred cover and its mean lightness (0 to 1). */
private class Wash(val image: ImageBitmap, val lightness: Float)

/** The cover at [WASH_PX] and blurred, or null until it loads (or when there's none). */
@Composable
private fun rememberWash(url: String?): Wash? {
    val context = LocalContext.current
    var wash by remember(url) { mutableStateOf<Wash?>(null) }
    LaunchedEffect(url) {
        if (url == null) return@LaunchedEffect
        val request = ImageRequest.Builder(context).data(url).size(WASH_PX).allowHardware(false).build()
        val drawable = (context.imageLoader.execute(request) as? SuccessResult)?.drawable ?: return@LaunchedEffect
        wash = withContext(Dispatchers.Default) {
            val blurred = blur(drawable.toBitmap())
            Wash(blurred.asImageBitmap(), lightness(blurred))
        }
    }
    return wash
}

/** Mean relative luminance, close enough to perceived lightness for deciding how much to dim. */
private fun lightness(bitmap: Bitmap): Float {
    val pixels = IntArray(bitmap.width * bitmap.height).also { bitmap.getPixels(it, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height) }
    return pixels.map { p -> (0.2126f * (p shr 16 and 0xFF) + 0.7152f * (p shr 8 and 0xFF) + 0.0722f * (p and 0xFF)) / 255f }.average().toFloat()
}

/** Three passes of a 5 px box blur, close to a Gaussian, over a bitmap of a few hundred pixels. */
private fun blur(source: Bitmap): Bitmap {
    val bitmap = Bitmap.createScaledBitmap(source, WASH_PX, WASH_PX, true).copy(Bitmap.Config.ARGB_8888, true)
    val w = bitmap.width
    val h = bitmap.height
    var pixels = IntArray(w * h).also { bitmap.getPixels(it, 0, w, 0, 0, w, h) }
    repeat(3) {
        pixels = boxPass(pixels, w, h, horizontal = true)
        pixels = boxPass(pixels, w, h, horizontal = false)
    }
    bitmap.setPixels(pixels, 0, w, 0, 0, w, h)
    return bitmap
}

private fun boxPass(input: IntArray, w: Int, h: Int, horizontal: Boolean, radius: Int = 2): IntArray {
    val output = IntArray(input.size)
    for (y in 0 until h) for (x in 0 until w) {
        var r = 0; var g = 0; var b = 0; var n = 0
        for (d in -radius..radius) {
            // Edges clamp, so the borders don't darken towards transparent black.
            val sx = if (horizontal) (x + d).coerceIn(0, w - 1) else x
            val sy = if (horizontal) y else (y + d).coerceIn(0, h - 1)
            val p = input[sy * w + sx]
            r += p shr 16 and 0xFF; g += p shr 8 and 0xFF; b += p and 0xFF; n++
        }
        output[y * w + x] = (0xFF shl 24) or (r / n shl 16) or (g / n shl 8) or (b / n)
    }
    return output
}
