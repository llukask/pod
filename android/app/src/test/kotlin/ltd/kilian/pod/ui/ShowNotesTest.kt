package ltd.kilian.pod.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class ShowNotesTest {
    @Test
    fun `paragraphs become blocks`() {
        assertEquals(
            listOf(NoteBlock("Money, explained."), NoteBlock("In this <b>episode</b> we talk.")),
            noteBlocks("<p>Money, explained.</p>\n<p>In this <b>episode</b> we talk.</p>"),
        )
    }

    @Test
    fun `list items carry their marker and number`() {
        assertEquals(
            listOf(
                NoteBlock("Intro"),
                NoteBlock("Why", marker = "•"),
                NoteBlock("What <a href=\"https://example.org\">else</a>", marker = "•"),
                NoteBlock("First", marker = "1."),
                NoteBlock("Second", marker = "2."),
            ),
            noteBlocks("<p>Intro</p><ul><li>Why</li><li>What <a href=\"https://example.org\">else</a></li></ul><ol><li>First</li><li>Second</li></ol>"),
        )
    }

    @Test
    fun `text outside blocks and line breaks stay`() {
        assertEquals(
            listOf(NoteBlock("Plain text<br>on two lines"), NoteBlock("Then a paragraph")),
            noteBlocks("Plain text<br>on two lines<p>Then a paragraph</p>"),
        )
    }

    @Test
    fun `empty blocks are dropped`() {
        assertEquals(listOf(NoteBlock("Kept")), noteBlocks("<p> </p><p>Kept</p><ul></ul>"))
    }
}
