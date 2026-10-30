package ltd.kilian.pod.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.fromHtml
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp

// ==============================================================================================
// Show notes
// ==============================================================================================
//
// Sanitized HTML (spec 2.2) at a reading size. Compose's `fromHtml` handles the inline markup
// (links, emphasis, line breaks) but runs paragraphs together and drops list markers, so the
// notes are first cut into blocks, each rendered on its own with room between paragraphs and a
// marker beside each list item, as the web's notes are.

/** One block of show notes: a paragraph's inline HTML, or a list item's with its marker ("•", "1."). */
data class NoteBlock(val html: String, val marker: String? = null)

private val BLOCK_TAG = Regex("<(/?)(p|div|ul|ol|li|h[1-6]|blockquote)\\b[^>]*>", RegexOption.IGNORE_CASE)
private val ANY_TAG = Regex("<[^>]*>")

/** Cuts sanitized HTML into blocks. Text outside any block is a paragraph of its own; empty blocks are dropped. */
fun noteBlocks(html: String): List<NoteBlock> {
    val blocks = mutableListOf<NoteBlock>()
    // Each open list: whether it's numbered, and the last number given.
    val lists = ArrayDeque<IntArray>()
    val buffer = StringBuilder()
    var marker: String? = null

    fun flush() {
        val inline = buffer.toString().trim()
        val text = ANY_TAG.replace(inline, "").replace("&nbsp;", " ").trim()
        if (text.isNotEmpty()) blocks += NoteBlock(inline, marker)
        buffer.clear()
        marker = null
    }

    var at = 0
    for (match in BLOCK_TAG.findAll(html)) {
        buffer.append(html, at, match.range.first)
        at = match.range.last + 1
        val closing = match.groupValues[1] == "/"
        when (match.groupValues[2].lowercase()) {
            "ul", "ol" -> {
                flush()
                if (closing) lists.removeLastOrNull() else lists.addLast(intArrayOf(if (match.groupValues[2].equals("ol", true)) 1 else 0, 0))
            }
            "li" -> {
                flush()
                if (!closing) {
                    val list = lists.lastOrNull()
                    marker = if (list != null && list[0] == 1) "${++list[1]}." else "•"
                }
            }
            else -> flush()
        }
    }
    buffer.append(html, at, html.length)
    flush()
    return blocks
}

@Composable
fun ShowNotes(html: String, modifier: Modifier = Modifier) {
    val colors = podColors
    val linkColor = colors.accentInk
    val blocks = remember(html) { noteBlocks(html) }
    // Ink softened towards the page, as the web's notes are, so a long read is gentler.
    val ink = lerp(colors.ink, colors.background, 0.12f)
    val links = TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline))
    Column(modifier.widthIn(max = 640.dp)) {
        blocks.forEachIndexed { index, block ->
            val text = remember(block, linkColor) { AnnotatedString.fromHtml(block.html, linkStyles = links) }
            // Paragraphs sit a line apart; items of one list sit closer.
            val gap = when {
                index == 0 -> 0.dp
                block.marker != null && blocks[index - 1].marker != null -> 6.dp
                else -> 16.dp
            }
            if (block.marker == null) {
                Text(text, style = PodType.reading, color = ink, modifier = Modifier.padding(top = gap))
            } else {
                Row(Modifier.padding(top = gap)) {
                    Text(block.marker, style = PodType.reading, color = colors.faint, modifier = Modifier.width(if (block.marker.length > 2) 28.dp else 22.dp).padding(start = 4.dp))
                    Text(text, style = PodType.reading, color = ink)
                }
            }
        }
    }
}
