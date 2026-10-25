//! Snapshots of `parse_feed` over trimmed real feeds and hand-written
//! malformed ones. `tests/fixtures/feeds/README.md` lists the sources.

use pod_protocol::{PodcastId, normalize_feed_url};
use pod_server::feed::{ContentHash, FeedParseError, ParsedFeed, parse_feed};

/// What a snapshot records: the parsed feed plus every hash ingest uses.
#[derive(Debug)]
#[allow(dead_code)] // Read through `Debug` only.
struct Snapshot {
    metadata_hash: ContentHash,
    feed_hash: ContentHash,
    episode_hashes: Vec<(String, ContentHash)>,
    feed: ParsedFeed,
}

fn parse_fixture(name: &str) -> Result<ParsedFeed, FeedParseError> {
    let path = format!(
        "{}/tests/fixtures/feeds/{name}.xml",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).expect("fixture file exists");
    // The fixture name stands in for the feed URL: only the podcast id and
    // the title fallback depend on it.
    let feed_url = format!("https://user:secret@feeds.example.com/{name}.xml");
    let podcast_id = PodcastId::from_feed_url(&normalize_feed_url(&feed_url).expect("valid URL"));
    parse_feed(podcast_id, &feed_url, &bytes)
}

macro_rules! snapshot_fixtures {
    ($($test:ident => $name:literal,)*) => {$(
        #[test]
        fn $test() {
            let feed = parse_fixture($name).expect("fixture is a feed");
            let snapshot = Snapshot {
                metadata_hash: feed.metadata.content_hash(),
                feed_hash: feed.feed_hash(),
                episode_hashes: feed
                    .episodes
                    .iter()
                    .map(|episode| (episode.guid.clone(), episode.content_hash()))
                    .collect(),
                feed,
            };
            insta::assert_debug_snapshot!($name, snapshot);
        }
    )*};
}

snapshot_fixtures! {
    art19_the_daily => "art19-the-daily",
    npr_planet_money => "npr-planet-money",
    bbc_global_news => "bbc-global-news",
    soundon_gooaye => "soundon-gooaye",
    backtracks_80000_hours => "backtracks-80000-hours",
    atom_podcast => "atom-podcast",
    malformed_missing_fields => "malformed-missing-fields",
    malformed_duplicate_guids => "malformed-duplicate-guids",
    malformed_show_notes => "malformed-show-notes",
}

#[test]
fn bodies_that_are_not_feeds_are_errors() {
    let podcast_id = PodcastId::from_feed_url(
        &normalize_feed_url("https://example.com/feed").expect("valid URL"),
    );
    for body in [
        &b""[..],
        b"<!doctype html><html><body>Not found</body></html>",
        b"<rss version=\"2.0\"><item>no channel</item></rss>",
        b"\x89PNG\r\n\x1a\n",
    ] {
        let result = parse_feed(podcast_id, "https://example.com/feed", body);
        assert!(
            matches!(result, Err(FeedParseError::NotAFeed(_))),
            "{:?} gave {result:?}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn a_panic_inside_feed_rs_is_an_error() {
    // `feed-rs` unwraps the parsed seconds of `<itunes:duration>`, which
    // overflows `u64` here.
    let body = r#"<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
        <channel><title>x</title><item><guid>a</guid>
        <itunes:duration>99999999999999999999999</itunes:duration>
        <enclosure url="https://example.com/a.mp3" type="audio/mpeg"/>
        </item></channel></rss>"#;
    let podcast_id = PodcastId::from_feed_url(
        &normalize_feed_url("https://example.com/feed").expect("valid URL"),
    );
    let result = parse_feed(podcast_id, "https://example.com/feed", body.as_bytes());
    assert!(
        matches!(result, Err(FeedParseError::ParserPanicked)),
        "{result:?}"
    );
}
