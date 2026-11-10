package ltd.kilian.pod.episode

import ltd.kilian.pod.core.Chapter
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

// Spec 17.4: chapter starts as M:SS or H:MM:SS, and the chapter that holds the position.

class ChaptersTest {
    @Test
    fun startsReadAsMinutesOrHours() {
        assertEquals("0:00", chapterStartLabel(0))
        assertEquals("0:59", chapterStartLabel(59_999))
        assertEquals("12:34", chapterStartLabel(754_500))
        assertEquals("1:00:00", chapterStartLabel(3_600_000))
        assertEquals("2:03:04", chapterStartLabel(7_384_000))
    }

    @Test
    fun theCurrentChapterIsTheLastToStartAtOrBeforeThePosition() {
        val chapters = listOf(Chapter(5_000, "Intro"), Chapter(60_000, "Interview"), Chapter(120_000, "Outro"))
        assertNull(currentChapter(chapters, 4_999))
        assertEquals(0, currentChapter(chapters, 5_000))
        assertEquals(1, currentChapter(chapters, 119_999))
        assertEquals(2, currentChapter(chapters, 999_000))
        assertNull(currentChapter(emptyList(), 0))
    }
}
