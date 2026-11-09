//! Episode chapters (spec 17).
//!
//! Every source (Podlove chapters in the feed, a `podcast:chapters` JSON
//! file, ID3 `CHAP` frames) yields [`RawChapter`]s, and [`normalize`] turns
//! them into the one list clients get.

use pod_protocol::Chapter;

/// Chapters kept per episode (spec 17.1).
pub const MAX_CHAPTERS: usize = 500;

/// A chapter as a source wrote it, before [`normalize`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawChapter {
    pub start_ms: Option<i64>,
    pub title: Option<String>,
    pub url: Option<String>,
    /// False for a JSON chapter marked `"toc": false`.
    pub toc: bool,
}

/// Applies spec 17.1: drops chapters without a usable start and those out of
/// the table of contents, sorts by start, keeps the first of each start, caps
/// the count, names untitled chapters and keeps only `http(s)` links.
pub fn normalize(raw: impl IntoIterator<Item = RawChapter>) -> Vec<Chapter> {
    let mut kept: Vec<(i64, RawChapter)> = raw
        .into_iter()
        .filter(|chapter| chapter.toc)
        .filter_map(|chapter| Some((chapter.start_ms.filter(|&ms| ms >= 0)?, chapter)))
        .collect();
    // Stable, so of two chapters with one start the one written first stays.
    kept.sort_by_key(|(start_ms, _)| *start_ms);
    kept.dedup_by_key(|(start_ms, _)| *start_ms);
    kept.truncate(MAX_CHAPTERS);
    kept.into_iter()
        .enumerate()
        .map(|(index, (start_ms, chapter))| Chapter {
            start_ms,
            title: chapter
                .title
                .map(|title| title.trim().to_owned())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| format!("Chapter {}", index + 1)),
            url: chapter.url.as_deref().and_then(crate::feed::http_url),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(start_ms: Option<i64>, title: &str) -> RawChapter {
        RawChapter {
            start_ms,
            title: Some(title.to_owned()),
            url: None,
            toc: true,
        }
    }

    #[test]
    fn sorts_dedups_and_names_chapters() {
        let chapters = normalize([
            raw(Some(60_000), "Second"),
            raw(Some(0), "  "),
            raw(Some(60_000), "Same start"),
            raw(None, "No start"),
            raw(Some(-1), "Negative"),
            RawChapter {
                toc: false,
                ..raw(Some(30_000), "Artwork change")
            },
            RawChapter {
                url: Some("javascript:alert(1)".to_owned()),
                ..raw(Some(90_000), "Third")
            },
        ]);
        let summary: Vec<_> = chapters
            .iter()
            .map(|chapter| {
                (
                    chapter.start_ms,
                    chapter.title.as_str(),
                    chapter.url.is_some(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (0, "Chapter 1", false),
                (60_000, "Second", false),
                (90_000, "Third", false)
            ]
        );
    }

    #[test]
    fn keeps_at_most_five_hundred() {
        let chapters = normalize((0..600).map(|n| raw(Some(n * 1000), "x")));
        assert_eq!(chapters.len(), MAX_CHAPTERS);
        assert_eq!(
            chapters.last().map(|chapter| chapter.start_ms),
            Some(499_000)
        );
    }
}
