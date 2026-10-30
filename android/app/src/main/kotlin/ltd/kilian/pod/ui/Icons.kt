package ltd.kilian.pod.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

// ==============================================================================================
// Icons
// ==============================================================================================
//
// The interface's icons, the web client's set (`web/src/ui/Icon.tsx`): a 24 unit grid, round
// 1.75 unit strokes so they sit beside Radio Canada's weight, and filled shapes for play and
// pause so they read at a glance inside small buttons. The Android-only ones (download, sleep,
// notes, link, the download states) are drawn the same way. `Icon` tints them, so they are
// built in black.

/** One stroke of an icon's drawing: an SVG path, stroked or filled. */
private class Part(val path: String, val filled: Boolean = false)

private fun stroke(path: String) = Part(path)
private fun fill(path: String) = Part(path, filled = true)

/** An SVG circle as path data, for [addPathNodes], which only reads paths. */
private fun circle(cx: Float, cy: Float, r: Float) = "M${cx - r},${cy}a$r,$r 0 1,0 ${2 * r},0a$r,$r 0 1,0 ${-2 * r},0Z"

/** An SVG `rect` with rounded corners as path data. */
private fun rect(x: Float, y: Float, w: Float, h: Float, r: Float) =
    "M${x + r},${y}h${w - 2 * r}a$r,$r 0 0 1 $r,${r}v${h - 2 * r}a$r,$r 0 0 1 ${-r},${r}h${-(w - 2 * r)}a$r,$r 0 0 1 ${-r},${-r}v${-(h - 2 * r)}a$r,$r 0 0 1 $r,${-r}Z"

private fun icon(name: String, vararg parts: Part): ImageVector {
    val builder = ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
    for (part in parts) {
        builder.addPath(
            pathData = addPathNodes(part.path),
            fill = if (part.filled) SolidColor(Color.Black) else null,
            stroke = if (part.filled) null else SolidColor(Color.Black),
            strokeLineWidth = 1.75f,
            strokeLineCap = StrokeCap.Round,
            strokeLineJoin = StrokeJoin.Round,
        )
    }
    return builder.build()
}

object PodIcons {
    val Play = icon("play", fill("M8 5.5v13a1 1 0 0 0 1.5.86l10.6-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5Z"))
    val Pause = icon("pause", fill(rect(6.5f, 5f, 4f, 14f, 1.2f)), fill(rect(13.5f, 5f, 4f, 14f, 1.2f)))
    val Check = icon("check", stroke("m5 12.5 4.5 4.5L19 7.5"))
    val Undo = icon("undo", stroke("M9 14 4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11"))
    val Archive = icon("archive", stroke(rect(3.5f, 4f, 17f, 4.5f, 1f)), stroke("M5.5 8.5V19a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1V8.5M10 12.5h4"))
    val Unarchive = icon("unarchive", stroke("M4 13v6a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-6"), stroke("M12 15V4M7.5 8.5 12 4l4.5 4.5"))

    /** A circular arrow back; the skip buttons print their seconds inside it. */
    val SkipBack = icon("skip-back", stroke("M4.5 12a7.5 7.5 0 1 0 2.2-5.3M4.5 4v4h4"))
    val SkipForward = icon("skip-forward", stroke("M19.5 12a7.5 7.5 0 1 1-2.2-5.3M19.5 4v4h-4"))
    val Close = icon("close", stroke("M6 6l12 12M18 6 6 18"))
    val More = icon("more", fill(circle(12f, 5.5f, 1.4f)), fill(circle(12f, 12f, 1.4f)), fill(circle(12f, 18.5f, 1.4f)))
    val Search = icon("search", stroke(circle(10.5f, 10.5f, 6f)), stroke("m15 15 5 5"))
    val Inbox = icon(
        "inbox",
        stroke("M3.5 13.5 6 5.5a1.5 1.5 0 0 1 1.4-1h9.2a1.5 1.5 0 0 1 1.4 1l2.5 8"),
        stroke("M3.5 13.5V18a1.5 1.5 0 0 0 1.5 1.5h14a1.5 1.5 0 0 0 1.5-1.5v-4.5h-5a3.5 3.5 0 0 1-7 0h-5Z"),
    )
    val Shows = icon(
        "shows",
        stroke(rect(4f, 4f, 7f, 7f, 1.5f)), stroke(rect(13f, 4f, 7f, 7f, 1.5f)),
        stroke(rect(4f, 13f, 7f, 7f, 1.5f)), stroke(rect(13f, 13f, 7f, 7f, 1.5f)),
    )
    val Settings = icon("settings", stroke("M4 7h9M17 7h3M4 17h3M11 17h9"), stroke(circle(15f, 7f, 2f)), stroke(circle(9f, 17f, 2f)))
    val External = icon("external", stroke("M14 4.5h5.5V10M19.5 4.5 11 13M17 14v4.5a1 1 0 0 1-1 1H5.5a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1H10"))
    val ChevronDown = icon("chevron-down", stroke("m6 9.5 6 6 6-6"))
    val ChevronRight = icon("chevron-right", stroke("m9.5 6 6 6-6 6"))
    val ArrowLeft = icon("arrow-left", stroke("M19 12H5M11 6l-6 6 6 6"))
    val Plus = icon("plus", stroke("M12 5v14M5 12h14"))
    val Minus = icon("minus", stroke("M5 12h14"))

    /** A show without a title yet: a sound wave. */
    val Waves = icon("waves", stroke("M4 10v4M8 7v10M12 4v16M16 8v8M20 11v2"))

    // Android only, in the same hand.

    /** The Downloads tab and the episode page's Download button: an arrow into a tray. */
    val Download = icon("download", stroke("M12 4v10.5M7.5 10 12 14.5 16.5 10"), stroke("M4.5 15.5v2.5a1.5 1.5 0 0 0 1.5 1.5h12a1.5 1.5 0 0 0 1.5-1.5v-2.5"))
    val Downloaded = icon("downloaded", stroke("m8 10 3 3 5.5-6"), stroke("M4.5 15.5v2.5a1.5 1.5 0 0 0 1.5 1.5h12a1.5 1.5 0 0 0 1.5-1.5v-2.5"))
    val Queued = icon("queued", stroke(circle(12f, 12f, 8.25f)), stroke("M12 7.5V12l3 2"))
    val Retry = icon("retry", stroke("M19.5 12a7.5 7.5 0 1 1-2.2-5.3M19.5 4v4h-4"))
    val Alert = icon("alert", stroke(circle(12f, 12f, 8.25f)), stroke("M12 7.75v5"), fill(circle(12f, 16.25f, 1.1f)))
    val Warning = icon("warning", stroke("M10.3 4.9 3.2 17.5A2 2 0 0 0 4.9 20.5h14.2a2 2 0 0 0 1.7-3L13.7 4.9a2 2 0 0 0-3.4 0Z"), stroke("M12 9.5v4"), fill(circle(12f, 16.75f, 1.05f)))
    val Gone = icon("gone", stroke(circle(12f, 12f, 8.25f)), stroke("m6.2 6.2 11.6 11.6"))
    val Sleep = icon("sleep", stroke("M19.5 14.5A7.5 7.5 0 0 1 9.5 4.5a7.5 7.5 0 1 0 10 10Z"))
    val Notes = icon("notes", stroke("M5 6.5h14M5 11h14M5 15.5h9"))
    val Link = icon("link", stroke("M10 14a4 4 0 0 0 5.66 0l3-3a4 4 0 0 0-5.66-5.66l-1 1"), stroke("M14 10a4 4 0 0 0-5.66 0l-3 3a4 4 0 0 0 5.66 5.66l1-1"))
    val Bell = icon("bell", stroke("M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15Z"), stroke("M10 20.5a2 2 0 0 0 4 0"))
    val Share = icon("share", stroke("M12 15V4M7.5 8.5 12 4l4.5 4.5"), stroke("M5 12.5V19a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-6.5"))
    val Stream = icon("stream", stroke("M7.5 16.5a6.5 6.5 0 0 1 0-9M16.5 7.5a6.5 6.5 0 0 1 0 9M4.5 19.5a10.5 10.5 0 0 1 0-15M19.5 4.5a10.5 10.5 0 0 1 0 15"), fill(circle(12f, 12f, 1.75f)))
}
