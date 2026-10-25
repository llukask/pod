//! Feed parsing and normalization (spec section 3.3).
//!
//! [`parse_feed`] turns a fetched body into the podcast metadata and episode
//! rows ingest stores, plus the hashes ingest compares
//! ([`ParsedFeed::feed_hash`], [`PodcastMetadata::content_hash`],
//! [`ParsedEpisode::content_hash`]). It does no I/O.
//!
//! Only a body that isn't a feed at all is an error. Everything else degrades:
//! a missing field becomes `None` or a fallback, and an item that can't
//! become an episode is skipped.

mod hash;
mod itunes;
mod sanitize;

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use feed_rs::model::{Entry, Feed, FeedType, Link, Text};
use pod_protocol::{
    EpisodeId, NormalizedFeedUrl, PodcastId, Timestamp, episode_key, normalize_feed_url,
};
use url::Url;

pub use hash::ContentHash;

/// A parsed and normalized feed.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFeed {
    pub metadata: PodcastMetadata,
    /// In feed order, with unique [`guid`](ParsedEpisode::guid)s.
    pub episodes: Vec<ParsedEpisode>,
    /// `<itunes:new-feed-url>`, if it holds an `http(s)` URL. The poller
    /// treats it like a permanent redirect (spec sections 1 and 3.2).
    pub new_feed_url: Option<NormalizedFeedUrl>,
}

/// The client-visible podcast fields from the feed (the catalog columns of
/// `podcast` in spec 2.2).
#[derive(Debug, Clone, PartialEq)]
pub struct PodcastMetadata {
    /// Never empty: falls back to the feed URL without its credentials.
    pub title: String,
    pub author: Option<String>,
    /// Sanitized HTML, like episode show notes, since publishers put markup
    /// here just as often. `None` when the feed has none.
    pub description: Option<String>,
    pub link: Option<String>,
    pub artwork_url: Option<String>,
    pub language: Option<String>,
}

/// One episode row (spec 2.2 `episode`), minus the server-managed columns.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEpisode {
    pub id: EpisodeId,
    /// The episode key (spec section 1): the trimmed guid, or the audio URL
    /// when the item has none.
    pub guid: String,
    pub title: String,
    /// The item's `description` (Atom `summary`), sanitized HTML.
    pub summary_html: String,
    /// The item's `content:encoded` (Atom `content`), sanitized HTML.
    pub content_html: String,
    pub link: Option<String>,
    /// An item without a date gets the Unix epoch, see [`published_at`].
    pub published_at: Timestamp,
    pub duration_ms: Option<i64>,
    pub audio_url: String,
    pub audio_type: String,
    pub audio_bytes: Option<i64>,
    pub artwork_url: Option<String>,
    pub season: Option<i32>,
    pub number: Option<i32>,
}

#[derive(Debug, thiserror::Error)]
pub enum FeedParseError {
    #[error("the body is not an RSS, Atom or JSON feed")]
    NotAFeed(#[source] feed_rs::parser::ParseFeedError),
    /// `feed-rs` has `unwrap`s on input-derived values (for example an
    /// `<itunes:duration>` with more digits than fit a `u64`).
    #[error("the feed parser panicked on this body")]
    ParserPanicked,
}

/// Parses `bytes`, fetched from `feed_url` for `podcast_id`.
///
/// `feed_url` only serves as the title of last resort. It is never logged,
/// since private feeds carry credentials in it.
pub fn parse_feed(
    podcast_id: PodcastId,
    feed_url: &str,
    bytes: &[u8],
) -> Result<ParsedFeed, FeedParseError> {
    // =========================================================================
    // Parse
    // =========================================================================

    // `feed-rs` invents ids for items without a guid, partly from a random
    // UUID. An empty id instead lets `episode_key` fall back to the audio URL,
    // which is stable across polls. Its own sanitizer is off because ours
    // runs on every text field below.
    let parser = feed_rs::parser::Builder::new()
        .id_generator(|_links, _title, _base| String::new())
        .sanitize_content(false)
        .build();
    // TODO: drop `catch_unwind` once `feed-rs` stops unwrapping parsed
    // numbers in `parse_npt`. Until then one junk `<itunes:duration>` fails
    // the whole poll, and the panic hook still prints the panic to stderr.
    let feed = catch_unwind(AssertUnwindSafe(|| parser.parse(bytes)))
        .map_err(|_| FeedParseError::ParserPanicked)?
        .map_err(FeedParseError::NotAFeed)?;

    let mut itunes = itunes::read(bytes).unwrap_or_else(|error| {
        tracing::warn!(%podcast_id, %error, "can't read iTunes tags from the feed");
        itunes::ItunesTags::default()
    });
    if itunes.items.len() != feed.entries.len() {
        // Pairing items with entries by position would be wrong, so no item
        // gets iTunes tags. JSON and RSS 1.0 feeds always end up here.
        if !itunes.items.is_empty() {
            tracing::warn!(
                %podcast_id,
                itunes_items = itunes.items.len(),
                entries = feed.entries.len(),
                "iTunes item count differs from the feed's, ignoring item tags"
            );
        }
        itunes.items.clear();
    }

    // =========================================================================
    // Normalize
    // =========================================================================

    // Atom and JSON Feed say whether a text is plain or HTML. RSS never
    // does, and `feed-rs` then labels it by element (`text/plain` for a
    // channel `<description>`, `text/html` for an item's), so for RSS the
    // content decides (see `sanitize::to_html`).
    let typed = matches!(feed.feed_type, FeedType::Atom | FeedType::JSON);
    let metadata = podcast_metadata(&feed, &itunes.channel, feed_url, typed);
    let new_feed_url = itunes
        .channel
        .new_feed_url
        .as_deref()
        .and_then(|url| normalize_feed_url(url).ok());

    let mut seen = HashSet::new();
    let mut episodes = Vec::with_capacity(feed.entries.len());
    let mut skipped = 0usize;
    for (index, entry) in feed.entries.iter().enumerate() {
        let tags = itunes.items.get(index);
        let Some(episode) = parse_episode(podcast_id, entry, tags, typed) else {
            skipped += 1;
            continue;
        };
        // Spec section 1: a repeated key is a feed bug, the first item wins.
        if !seen.insert(episode.guid.clone()) {
            tracing::warn!(
                %podcast_id,
                episode_id = %episode.id,
                "feed repeats an episode key, keeping the first item"
            );
            continue;
        }
        episodes.push(episode);
    }
    if skipped > 0 {
        tracing::debug!(%podcast_id, skipped, "skipped items without an audio enclosure");
    }

    Ok(ParsedFeed {
        metadata,
        episodes,
        new_feed_url,
    })
}

// =============================================================================
// Podcast metadata
// =============================================================================

fn podcast_metadata(
    feed: &Feed,
    itunes: &itunes::ItunesChannel,
    feed_url: &str,
    typed: bool,
) -> PodcastMetadata {
    let title = feed
        .title
        .as_ref()
        .and_then(|title| non_empty(&title.content))
        .unwrap_or_else(|| without_credentials(feed_url));
    let author = feed
        .authors
        .iter()
        .find_map(|person| non_empty(person.name.as_deref()?));
    let description = feed
        .description
        .as_ref()
        .and_then(|text| text_to_html(text, typed));
    let link = page_link(&feed.links);
    // Apple requires `<itunes:image>` to be at least 1400 px square, while
    // RSS `<image>` is capped at 144 px wide, so the iTunes one wins.
    let artwork_url = [
        itunes.image.as_deref(),
        feed.logo.as_ref().map(|image| image.uri.as_str()),
        feed.icon.as_ref().map(|image| image.uri.as_str()),
    ]
    .into_iter()
    .flatten()
    .find_map(http_url);
    let language = feed.language.as_deref().and_then(non_empty);
    PodcastMetadata {
        title,
        author,
        description,
        link,
        artwork_url,
        language,
    }
}

/// The feed URL as a display title. Credentials are dropped because the
/// title is sent to every subscriber.
fn without_credentials(feed_url: &str) -> String {
    match Url::parse(feed_url.trim()) {
        Ok(mut url) => {
            // Both setters only fail for URLs that can't have credentials,
            // which then have none to remove.
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.into()
        }
        Err(_) => feed_url.trim().to_owned(),
    }
}

// =============================================================================
// Episodes
// =============================================================================

/// Builds the episode for `entry`, or `None` if it has no usable audio
/// enclosure.
fn parse_episode(
    podcast_id: PodcastId,
    entry: &Entry,
    tags: Option<&itunes::ItunesItem>,
    typed: bool,
) -> Option<ParsedEpisode> {
    let enclosure = audio_enclosure(entry)?;
    let guid = episode_key(Some(&entry.id), &enclosure.url).to_owned();

    let title = entry
        .title
        .as_ref()
        .into_iter()
        .chain(entry.media.iter().filter_map(|media| media.title.as_ref()))
        .find_map(|title| non_empty(&title.content))
        .unwrap_or_else(|| file_name(&enclosure.url));

    // `itunes:summary` stands in when there's no `description`.
    let summary_html = entry
        .summary
        .as_ref()
        .or_else(|| {
            entry
                .media
                .iter()
                .find_map(|media| media.description.as_ref())
        })
        .and_then(|text| text_to_html(text, typed))
        .unwrap_or_default();
    let content_html = entry
        .content
        .as_ref()
        .and_then(|content| {
            let body = content.body.as_deref()?;
            non_empty(&sanitize::to_html(
                body,
                typed && is_plain(content.content_type.essence()),
            ))
        })
        .unwrap_or_default();

    let duration_ms = tags
        .and_then(|tags| tags.duration.as_deref())
        .and_then(parse_duration_ms)
        .or(enclosure.duration_ms);

    let artwork_url = tags
        .and_then(|tags| tags.image.as_deref())
        .and_then(http_url)
        .or_else(|| {
            entry
                .media
                .iter()
                .flat_map(|media| &media.thumbnails)
                .find_map(|thumbnail| http_url(&thumbnail.image.uri))
        });

    let season = tags
        .and_then(|tags| parse_count(tags.season.as_deref()?))
        .or_else(|| {
            let season = entry.media.iter().find_map(|media| media.season.as_ref())?;
            i32::try_from(season.number).ok()
        });
    let number = tags
        .and_then(|tags| parse_count(tags.episode.as_deref()?))
        .or_else(|| {
            let episode = entry
                .media
                .iter()
                .find_map(|media| media.episode.as_ref())?;
            let number = episode.number;
            // `feed-rs` parses `<podcast:episode>` as a float; only whole
            // numbers map to the integer column.
            (number.fract() == 0.0 && (0.0..=i32::MAX as f32).contains(&number))
                .then_some(number as i32)
        });

    Some(ParsedEpisode {
        id: EpisodeId::new(podcast_id, &guid),
        guid,
        title,
        summary_html,
        content_html,
        link: page_link(&entry.links),
        published_at: published_at(entry),
        duration_ms,
        audio_url: enclosure.url,
        audio_type: enclosure.mime,
        audio_bytes: enclosure.bytes,
        artwork_url,
        season,
        number,
    })
}

/// The item's date, else the Unix epoch.
///
/// The fallback has to be the same on every poll, or the episode hash would
/// churn. The feed's own `pubDate` or `lastBuildDate` would churn: both move
/// whenever the publisher rebuilds the feed. The epoch also sorts undated
/// items last, which is where an unknown date belongs.
// TODO: ingest could keep the first-seen time for undated episodes instead,
// if the epoch turns out to confuse users.
fn published_at(entry: &Entry) -> Timestamp {
    entry
        .published
        .or(entry.updated)
        .and_then(|date| Timestamp::from_epoch_ms(date.timestamp_millis()).ok())
        .unwrap_or_else(|| Timestamp::from_epoch_ms(0).expect("the epoch is in range"))
}

/// The last path segment of the audio URL, percent-decoded, as the title of
/// an item that has none.
fn file_name(audio_url: &str) -> String {
    let segment = Url::parse(audio_url)
        .ok()
        .and_then(|url| {
            url.path_segments()?
                .rev()
                .find(|segment| !segment.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let decoded = percent_encoding::percent_decode_str(&segment).decode_utf8_lossy();
    non_empty(&decoded).unwrap_or_else(|| audio_url.to_owned())
}

// =============================================================================
// Audio enclosure
// =============================================================================

#[derive(Debug)]
struct Enclosure {
    url: String,
    mime: String,
    bytes: Option<i64>,
    duration_ms: Option<i64>,
}

/// The first enclosure that is audio, or one of the video types podcast apps
/// play (spec 3.3).
///
/// RSS `<enclosure>` and Media RSS `<media:content>` both land in
/// `entry.media`. Atom puts enclosures in `<link rel="enclosure">`.
// TODO: `feed-rs` drops the RSS `<enclosure>` when `<media:content>` is
// present, so a feed listing alternate bitrates in Media RSS gets its first
// playable `media:content` rather than its `enclosure`.
fn audio_enclosure(entry: &Entry) -> Option<Enclosure> {
    let media = entry
        .media
        .iter()
        .flat_map(|media| &media.content)
        .filter_map(|content| {
            let url = content.url.as_ref()?;
            let mime = content.content_type.as_ref().map(ToString::to_string);
            let duration_ms = content
                .duration
                .and_then(|duration| i64::try_from(duration.as_millis()).ok())
                .filter(|&ms| ms > 0);
            playable(url.as_str(), mime.as_deref(), content.size, duration_ms)
        });
    let links = entry
        .links
        .iter()
        .filter(|link| link.rel.as_deref() == Some("enclosure"))
        .filter_map(|link| playable(&link.href, link.media_type.as_deref(), link.length, None));
    media.chain(links).next()
}

fn playable(
    url: &str,
    declared_mime: Option<&str>,
    size: Option<u64>,
    duration_ms: Option<i64>,
) -> Option<Enclosure> {
    let url = http_url(url)?;
    let declared = declared_mime
        .and_then(|mime| mime.split(';').next())
        .map(|mime| mime.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let mime = match declared.as_str() {
        // Hosts serving from S3 and the like often send a generic type.
        "" | "application/octet-stream" | "binary/octet-stream" => mime_from_extension(&url)?,
        _ => declared,
    };
    let is_playable = mime.starts_with("audio/") || mime == "video/mp4" || mime == "video/x-m4v";
    is_playable.then(|| Enclosure {
        url,
        mime,
        // Hosts that don't know the size write placeholders such as `0` or
        // `1`. No real episode is under a kibibyte.
        bytes: size
            .and_then(|size| i64::try_from(size).ok())
            .filter(|&size| size >= 1024),
        duration_ms,
    })
}

fn mime_from_extension(url: &str) -> Option<String> {
    let path = Url::parse(url).ok()?.path().to_ascii_lowercase();
    let (_, extension) = path.rsplit_once('.')?;
    let mime = match extension {
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        _ => return None,
    };
    Some(mime.to_owned())
}

// =============================================================================
// Field helpers
// =============================================================================

/// Parses `<itunes:duration>`: seconds, `MM:SS` or `HH:MM:SS`, each with
/// optional fractional seconds. Zero, which some hosts write for "unknown",
/// is `None` too.
fn parse_duration_ms(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let (last, leading) = parts.split_last()?;
    let (seconds, fraction) = last.split_once('.').unwrap_or((last, ""));

    let mut total_seconds = 0u64;
    for part in leading.iter().chain([&seconds]) {
        total_seconds = total_seconds
            .checked_mul(60)?
            .checked_add(parse_digits(part)?)?;
    }
    let fraction_ms = if fraction.is_empty() {
        0
    } else {
        // Milliseconds are the first three digits, padded: ".5" is 500.
        let padded = format!("{fraction:0<3}");
        parse_digits(fraction)?;
        padded[..3].parse::<u64>().ok()?
    };
    let ms = total_seconds.checked_mul(1000)?.checked_add(fraction_ms)?;
    i64::try_from(ms).ok().filter(|&ms| ms > 0)
}

/// ASCII digits only: `u64::from_str` would also take a leading `+`.
fn parse_digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A season or episode number: a non-negative integer.
fn parse_count(text: &str) -> Option<i32> {
    let value = parse_digits(text.trim())?;
    i32::try_from(value).ok()
}

/// A feed text field as sanitized HTML, or `None` when it's empty. `typed`
/// says whether the feed format declares text types at all.
fn text_to_html(text: &Text, typed: bool) -> Option<String> {
    let declared_plain = typed && is_plain(text.content_type.essence());
    non_empty(&sanitize::to_html(&text.content, declared_plain))
}

fn is_plain(essence: impl std::fmt::Display) -> bool {
    essence.to_string() == "text/plain"
}

/// The page link among `links`: the first one without a `rel`, or with
/// `rel="alternate"`. That skips Atom `self` and `hub` links, enclosures and
/// comment links.
fn page_link(links: &[Link]) -> Option<String> {
    links
        .iter()
        .filter(|link| link.target.is_none())
        .filter(|link| matches!(link.rel.as_deref(), None | Some("alternate")))
        .find_map(|link| http_url(&link.href))
}

/// `text` if it is an absolute `http` or `https` URL. Clients turn links into
/// anchors and fetch artwork and audio, so `javascript:`, `data:` and
/// relative URLs are dropped.
fn http_url(text: &str) -> Option<String> {
    let url = Url::parse(text.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_in_common_forms() {
        let cases = [
            ("1991", Some(1_991_000)),
            ("12:34", Some(754_000)),
            ("75:30", Some(4_530_000)),
            ("1:02:03", Some(3_723_000)),
            ("01:02:03.5", Some(3_723_500)),
            ("3600.25", Some(3_600_250)),
            (" 42 ", Some(42_000)),
            ("0", None),
            ("00:00:00", None),
            ("", None),
            ("1:2:3:4", None),
            ("abc", None),
            ("1h 2m", None),
            ("-5", None),
            ("+5", None),
            ("1.2.3", None),
            ("99999999999999999999999", None),
            ("١٢", None),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_duration_ms(text), expected, "{text:?}");
        }
    }

    #[test]
    fn mime_inferred_only_for_generic_types() {
        let enclosure = playable(
            "https://cdn.example.com/ep.M4A?token=1",
            Some("application/octet-stream"),
            Some(0),
            None,
        )
        .expect("m4a is audio");
        assert_eq!(enclosure.mime, "audio/mp4");
        assert_eq!(enclosure.bytes, None);

        assert!(
            playable(
                "https://cdn.example.com/ep.mp3",
                Some("image/jpeg"),
                None,
                None
            )
            .is_none()
        );
        assert!(playable("https://cdn.example.com/ep", None, None, None).is_none());
        assert!(
            playable(
                "ftp://cdn.example.com/ep.mp3",
                Some("audio/mpeg"),
                None,
                None
            )
            .is_none()
        );
        assert_eq!(
            playable(
                "https://e.com/a",
                Some("Audio/MPEG; charset=x"),
                Some(10),
                None
            )
            .expect("declared audio")
            .mime,
            "audio/mpeg"
        );
    }

    #[test]
    fn title_fallback_drops_credentials() {
        assert_eq!(
            without_credentials("https://user:secret@example.com/feed.xml"),
            "https://example.com/feed.xml"
        );
    }
}
