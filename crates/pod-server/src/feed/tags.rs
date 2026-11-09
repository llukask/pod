//! A second, streaming pass over the feed XML for the namespaced tags
//! `feed-rs` drops or misreads: iTunes, Podcasting 2.0 (`podcast:`) and
//! Podlove Simple Chapters (`psc:`).
//!
//! `feed-rs` ignores `<itunes:new-feed-url>`, `<itunes:type>`,
//! `<itunes:episodeType>`, `<podcast:guid>`, `<podcast:chapters>` and
//! `<psc:chapters>`, lets a small RSS `<image>` win over `<itunes:image>`,
//! reads `<itunes:duration>` with an unanchored regex (so `12:34` becomes 12
//! seconds), and doesn't look at iTunes tags in Atom at all. This pass reads
//! the raw text of just those tags.
//!
//! Items are matched to `feed-rs` entries by position. The pass counts
//! exactly the elements `feed-rs` turns into entries (`<item>` children of the
//! first `<channel>` under `<rss>`, `<entry>` children of an Atom `<feed>`),
//! and the caller drops the per-item results if the counts disagree, so a
//! mismatch degrades to missing durations rather than shifted ones.

use quick_xml::NsReader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;

const ITUNES_NS: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";
/// The Podcasting 2.0 namespace, and the GitHub URL early feeds declared.
const PODCAST_NS: [&str; 2] = [
    "https://podcastindex.org/namespace/1.0",
    "https://github.com/Podcastindex-org/podcast-namespace/blob/main/docs/1.0.md",
];
const PSC_NS: &str = "http://podlove.org/simple-chapters";

/// The tags found in one feed. Values are trimmed and non-empty; when a tag
/// repeats, the first one wins.
#[derive(Debug, Default)]
pub(super) struct FeedTags {
    pub channel: ChannelTags,
    /// One per feed entry, in document order.
    pub items: Vec<ItemTags>,
}

#[derive(Debug, Default)]
pub(super) struct ChannelTags {
    pub new_feed_url: Option<String>,
    pub image: Option<String>,
    /// `<itunes:type>`.
    pub show_type: Option<String>,
    /// `<podcast:guid>`.
    pub guid: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct ItemTags {
    pub duration: Option<String>,
    pub image: Option<String>,
    pub season: Option<String>,
    pub episode: Option<String>,
    /// `<itunes:episodeType>`.
    pub episode_type: Option<String>,
    /// The `url` attribute of `<podcast:chapters>`.
    pub chapters_url: Option<String>,
    /// The `<psc:chapter>` elements of the item's first `<psc:chapters>`,
    /// in document order.
    pub psc_chapters: Vec<PscChapter>,
}

/// One `<psc:chapter>`, its attributes as written.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct PscChapter {
    pub start: Option<String>,
    pub title: Option<String>,
    pub href: Option<String>,
}

/// The namespaces this pass reads.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ns {
    Itunes,
    Podcast,
    Psc,
    Other,
}

/// Where the reader is, as far as this pass cares.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Frame {
    RssRoot,
    AtomRoot,
    Channel,
    Item,
    /// Inside the item's first `<psc:chapters>`.
    PscChapters,
    /// Collecting the text of a tag into `text`.
    Capture(Field),
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    NewFeedUrl,
    ShowType,
    Guid,
    Duration,
    Season,
    Episode,
    EpisodeType,
}

pub(super) fn read(bytes: &[u8]) -> Result<FeedTags, quick_xml::Error> {
    let mut reader = NsReader::from_reader(bytes);
    // `<itunes:image href="..."/>` then arrives as a start and an end event,
    // so every element pushes and pops exactly one frame.
    reader.config_mut().expand_empty_elements = true;

    let mut tags = FeedTags::default();
    let mut stack: Vec<Frame> = Vec::new();
    let mut seen_channel = false;
    let mut text = String::new();
    let mut buf = Vec::new();

    loop {
        let (namespace, event) = reader.read_resolved_event_into(&mut buf)?;
        // Some feeds use a prefix without declaring it, and a few declare
        // the URI in a different case.
        let ns = match &namespace {
            ResolveResult::Bound(ns) => {
                let uri = ns.0;
                if uri.eq_ignore_ascii_case(ITUNES_NS) {
                    Ns::Itunes
                } else if PODCAST_NS
                    .iter()
                    .any(|known| uri.eq_ignore_ascii_case(known))
                {
                    Ns::Podcast
                } else if uri.eq_ignore_ascii_case(PSC_NS) {
                    Ns::Psc
                } else {
                    Ns::Other
                }
            }
            ResolveResult::Unknown(prefix) => match prefix.as_ref() {
                "itunes" => Ns::Itunes,
                "podcast" => Ns::Podcast,
                "psc" => Ns::Psc,
                _ => Ns::Other,
            },
            ResolveResult::Unbound => Ns::Other,
        };
        match event {
            Event::Start(element) => {
                let parent = stack.last().copied();
                let local = element.local_name();
                let frame = match (parent, element.name().0, ns, local.as_ref()) {
                    (None, "rss", ..) => Frame::RssRoot,
                    (None, "feed", ..) => Frame::AtomRoot,
                    (Some(Frame::RssRoot), "channel", ..) if !seen_channel => {
                        seen_channel = true;
                        Frame::Channel
                    }
                    (Some(Frame::Channel), "item", ..) | (Some(Frame::AtomRoot), "entry", ..) => {
                        tags.items.push(ItemTags::default());
                        Frame::Item
                    }
                    (Some(Frame::Channel | Frame::AtomRoot), _, Ns::Itunes, "new-feed-url") => {
                        Frame::Capture(Field::NewFeedUrl)
                    }
                    (Some(Frame::Channel | Frame::AtomRoot), _, Ns::Itunes, "type") => {
                        Frame::Capture(Field::ShowType)
                    }
                    (Some(Frame::Channel | Frame::AtomRoot), _, Ns::Podcast, "guid") => {
                        Frame::Capture(Field::Guid)
                    }
                    (Some(Frame::Channel | Frame::AtomRoot), _, Ns::Itunes, "image") => {
                        set_once(&mut tags.channel.image, attribute(&element, "href"));
                        Frame::Other
                    }
                    (Some(Frame::Item), _, Ns::Itunes, "duration") => {
                        Frame::Capture(Field::Duration)
                    }
                    (Some(Frame::Item), _, Ns::Itunes, "season") => Frame::Capture(Field::Season),
                    (Some(Frame::Item), _, Ns::Itunes, "episode") => Frame::Capture(Field::Episode),
                    (Some(Frame::Item), _, Ns::Itunes, "episodeType") => {
                        Frame::Capture(Field::EpisodeType)
                    }
                    (Some(Frame::Item), _, Ns::Itunes, "image") => {
                        if let Some(item) = tags.items.last_mut() {
                            set_once(&mut item.image, attribute(&element, "href"));
                        }
                        Frame::Other
                    }
                    (Some(Frame::Item), _, Ns::Podcast, "chapters") => {
                        if let Some(item) = tags.items.last_mut() {
                            set_once(&mut item.chapters_url, attribute(&element, "url"));
                        }
                        Frame::Other
                    }
                    // Only the first `<psc:chapters>` of an item counts.
                    (Some(Frame::Item), _, Ns::Psc, "chapters")
                        if tags
                            .items
                            .last()
                            .is_some_and(|item| item.psc_chapters.is_empty()) =>
                    {
                        Frame::PscChapters
                    }
                    (Some(Frame::PscChapters), _, Ns::Psc, "chapter") => {
                        if let Some(item) = tags.items.last_mut() {
                            item.psc_chapters.push(PscChapter {
                                start: attribute(&element, "start"),
                                title: attribute(&element, "title"),
                                href: attribute(&element, "href"),
                            });
                        }
                        Frame::Other
                    }
                    _ => Frame::Other,
                };
                if matches!(frame, Frame::Capture(_)) {
                    text.clear();
                }
                stack.push(frame);
            }
            Event::End(_) => {
                if let Some(Frame::Capture(field)) = stack.pop() {
                    let value = Some(text.trim().to_owned());
                    let item = tags.items.last_mut();
                    let slot = match field {
                        Field::NewFeedUrl => Some(&mut tags.channel.new_feed_url),
                        Field::ShowType => Some(&mut tags.channel.show_type),
                        Field::Guid => Some(&mut tags.channel.guid),
                        Field::Duration => item.map(|item| &mut item.duration),
                        Field::Season => item.map(|item| &mut item.season),
                        Field::Episode => item.map(|item| &mut item.episode),
                        Field::EpisodeType => item.map(|item| &mut item.episode_type),
                    };
                    if let Some(slot) = slot {
                        set_once(slot, value);
                    }
                }
            }
            Event::Text(chunk) if capturing(&stack) => text.push_str(&chunk.xml10_content()),
            Event::CData(chunk) if capturing(&stack) => text.push_str(&chunk.xml10_content()),
            Event::GeneralRef(reference) if capturing(&stack) => {
                // Text arrives split around entity references (`&amp;` in a
                // URL's query string). Unknown entities are dropped.
                if let Ok(Some(ch)) = reference.resolve_char_ref() {
                    text.push(ch);
                } else if let Some(resolved) = resolve_predefined_entity(&reference) {
                    text.push_str(resolved);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(tags)
}

fn capturing(stack: &[Frame]) -> bool {
    matches!(stack.last(), Some(Frame::Capture(_)))
}

/// The trimmed value of the attribute `name`, entities resolved.
fn attribute(element: &BytesStart<'_>, name: &str) -> Option<String> {
    let attribute = element.try_get_attribute(name).ok().flatten()?;
    let value = attribute
        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
        .ok()?;
    Some(value.trim().to_owned())
}

/// Keeps the first non-empty value.
fn set_once(slot: &mut Option<String>, value: Option<String>) {
    if slot.is_none() {
        *slot = value.filter(|value| !value.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_channel_and_item_tags_in_rss() {
        let xml = r#"<?xml version="1.0"?>
            <rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
            <channel>
              <itunes:new-feed-url> https://example.com/new?a=1&amp;b=2 </itunes:new-feed-url>
              <itunes:image href="https://example.com/art.jpg"/>
              <item><itunes:duration>12:34</itunes:duration><itunes:season>2</itunes:season></item>
              <item><itunes:episode><![CDATA[7]]></itunes:episode><itunes:image href="https://example.com/ep.jpg"/></item>
            </channel></rss>"#;
        let tags = read(xml.as_bytes()).expect("well-formed XML");
        assert_eq!(
            tags.channel.new_feed_url.as_deref(),
            Some("https://example.com/new?a=1&b=2")
        );
        assert_eq!(
            tags.channel.image.as_deref(),
            Some("https://example.com/art.jpg")
        );
        assert_eq!(tags.items.len(), 2);
        assert_eq!(tags.items[0].duration.as_deref(), Some("12:34"));
        assert_eq!(tags.items[0].season.as_deref(), Some("2"));
        assert_eq!(tags.items[1].episode.as_deref(), Some("7"));
        assert_eq!(
            tags.items[1].image.as_deref(),
            Some("https://example.com/ep.jpg")
        );
    }

    #[test]
    fn accepts_an_undeclared_itunes_prefix() {
        let xml = r#"<rss version="2.0"><channel>
            <item><itunes:duration>61</itunes:duration></item>
            </channel></rss>"#;
        let tags = read(xml.as_bytes()).expect("well-formed XML");
        assert_eq!(tags.items[0].duration.as_deref(), Some("61"));
    }

    #[test]
    fn counts_atom_entries() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"
                xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
            <itunes:image href="https://example.com/art.jpg"/>
            <entry><itunes:duration>1:00:00</itunes:duration></entry>
            <entry/>
            </feed>"#;
        let tags = read(xml.as_bytes()).expect("well-formed XML");
        assert_eq!(
            tags.channel.image.as_deref(),
            Some("https://example.com/art.jpg")
        );
        assert_eq!(tags.items.len(), 2);
        assert_eq!(tags.items[0].duration.as_deref(), Some("1:00:00"));
    }

    #[test]
    fn ignores_tags_outside_items_and_nested_ones() {
        let xml = r#"<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
            <channel>
              <itunes:duration>99</itunes:duration>
              <item><group><itunes:duration>5</itunes:duration></group></item>
            </channel></rss>"#;
        let tags = read(xml.as_bytes()).expect("well-formed XML");
        assert_eq!(tags.items.len(), 1);
        assert_eq!(tags.items[0].duration, None);
    }
}
