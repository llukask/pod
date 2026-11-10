//! ID3v2 `CHAP` frames at the start of an audio file (spec 17.2).
//!
//! The server reads the 10-byte tag header first, learns the tag's size
//! from it, and then reads exactly the tag, so it never downloads audio.

use std::io::Cursor;

use super::RawChapter;

/// Tags larger than this are skipped: chapter frames take a few kilobytes,
/// so a tag this big is mostly artwork.
pub const MAX_TAG_BYTES: usize = 4 * 1024 * 1024;

/// The bytes of an ID3v2 header.
pub const HEADER_BYTES: usize = 10;

/// The whole tag's size in bytes, header and footer included, if `header`
/// starts with an ID3v2.3 or v2.4 header. Version 2.2 has no `CHAP` frame.
pub fn tag_size(header: &[u8]) -> Option<usize> {
    let header: &[u8; HEADER_BYTES] = header.get(..HEADER_BYTES)?.try_into().ok()?;
    if &header[..3] != b"ID3" || !matches!(header[3], 3 | 4) {
        return None;
    }
    // The size is "synchsafe": four bytes of seven bits each.
    let size_bytes = &header[6..10];
    if size_bytes.iter().any(|byte| byte & 0x80 != 0) {
        return None;
    }
    let size = size_bytes
        .iter()
        .fold(0usize, |size, &byte| (size << 7) | usize::from(byte));
    let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
    Some(HEADER_BYTES + size + footer)
}

/// The chapters of a complete ID3v2 tag. A tag that doesn't parse has none.
pub fn chapters(tag: &[u8]) -> Vec<RawChapter> {
    let Ok(tag) = id3::Tag::read_from2(Cursor::new(tag)) else {
        return Vec::new();
    };
    tag.chapters()
        .map(|chapter| RawChapter {
            start_ms: Some(i64::from(chapter.start_time)),
            title: chapter
                .frames
                .iter()
                .find(|frame| frame.id() == "TIT2")
                .and_then(|frame| frame.content().text())
                .map(str::to_owned),
            url: chapter
                .frames
                .iter()
                .find_map(|frame| frame.content().extended_link())
                .map(|link| link.link.clone()),
            toc: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use id3::frame::{Chapter, Content, ExtendedLink, Frame};
    use id3::{TagLike, Version};

    use super::*;

    fn tag_bytes(version: Version) -> Vec<u8> {
        let mut tag = id3::Tag::new();
        tag.set_title("Episode 1");
        for (id, start, title) in [("ch1", 61_000, "Second"), ("ch0", 0, "Intro")] {
            let mut frames = vec![Frame::text("TIT2", title)];
            if id == "ch1" {
                frames.push(Frame::with_content(
                    "WXXX",
                    Content::ExtendedLink(ExtendedLink {
                        description: String::new(),
                        link: "https://example.com/second".to_owned(),
                    }),
                ));
            }
            tag.add_frame(Chapter {
                element_id: id.to_owned(),
                start_time: start,
                end_time: start + 1000,
                start_offset: u32::MAX,
                end_offset: u32::MAX,
                frames,
            });
        }
        let mut bytes = Vec::new();
        tag.write_to(&mut bytes, version)
            .expect("an in-memory tag writes");
        bytes
    }

    #[test]
    fn reads_chapters_with_titles_and_links() {
        for version in [Version::Id3v23, Version::Id3v24] {
            let bytes = tag_bytes(version);
            assert_eq!(tag_size(&bytes), Some(bytes.len()), "{version:?}");
            let mut found = chapters(&bytes);
            found.sort_by_key(|chapter| chapter.start_ms);
            assert_eq!(
                found,
                [
                    RawChapter {
                        start_ms: Some(0),
                        title: Some("Intro".to_owned()),
                        url: None,
                        toc: true,
                    },
                    RawChapter {
                        start_ms: Some(61_000),
                        title: Some("Second".to_owned()),
                        url: Some("https://example.com/second".to_owned()),
                        toc: true,
                    },
                ],
                "{version:?}"
            );
        }
    }

    #[test]
    fn only_v23_and_v24_headers_have_a_size() {
        assert_eq!(tag_size(b"ID3\x04\x00\x00\x00\x00\x02\x01"), Some(10 + 257));
        assert_eq!(tag_size(b"ID3\x04\x00\x10\x00\x00\x00\x00"), Some(20));
        assert_eq!(tag_size(b"ID3\x02\x00\x00\x00\x00\x02\x01"), None);
        assert_eq!(tag_size(b"ID3\x04\x00\x00\x80\x00\x00\x00"), None);
        assert_eq!(tag_size(b"\xff\xfb\x90\x00"), None);
        assert_eq!(tag_size(b"ID3"), None);
    }

    #[test]
    fn a_broken_tag_has_no_chapters() {
        assert!(chapters(b"ID3\x04\x00\x00\x00\x00\x00\x30garbage").is_empty());
    }
}
