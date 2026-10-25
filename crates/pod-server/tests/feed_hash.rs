use pod_protocol::PodcastId;
use pod_server::feed::{ContentHash, ParsedFeed, parse_feed};

const FEED_URL: &str = "https://example.com/feed.xml";

const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
<channel>
  <title>Hash Test</title>
  <lastBuildDate>Mon, 20 Oct 2026 05:00:00 GMT</lastBuildDate>
  <item>
    <title>First</title>
    <guid>ep-1</guid>
    <pubDate>Sun, 19 Oct 2026 05:00:00 GMT</pubDate>
    <enclosure url="https://cdn.example.com/1.mp3" type="audio/mpeg" length="1000"/>
  </item>
  <item>
    <title>Second</title>
    <guid>ep-2</guid>
    <pubDate>Mon, 20 Oct 2026 05:00:00 GMT</pubDate>
    <enclosure url="https://cdn.example.com/2.mp3" type="audio/mpeg" length="2000"/>
  </item>
</channel>
</rss>"#;

fn parse(xml: &str) -> ParsedFeed {
    let podcast_id = PodcastId::from_feed_url(
        &pod_protocol::normalize_feed_url(FEED_URL).expect("literal URL is valid"),
    );
    parse_feed(podcast_id, FEED_URL, xml.as_bytes()).expect("test feed parses")
}

fn feed_hash(xml: &str) -> ContentHash {
    parse(xml).feed_hash()
}

#[test]
fn last_build_date_does_not_change_the_feed_hash() {
    let rebuilt = FEED.replace(
        "<lastBuildDate>Mon, 20 Oct 2026 05:00:00 GMT",
        "<lastBuildDate>Sat, 25 Oct 2026 12:34:56 GMT",
    );
    assert_ne!(rebuilt, FEED);
    assert_eq!(feed_hash(&rebuilt), feed_hash(FEED));
}

#[test]
fn an_episode_title_changes_the_feed_and_episode_hash() {
    let retitled = FEED.replace("<title>Second</title>", "<title>Second, fixed</title>");
    assert_ne!(feed_hash(&retitled), feed_hash(FEED));

    let (before, after) = (parse(FEED), parse(&retitled));
    assert_eq!(
        before.episodes[0].content_hash(),
        after.episodes[0].content_hash()
    );
    assert_ne!(
        before.episodes[1].content_hash(),
        after.episodes[1].content_hash()
    );
    assert_eq!(
        before.metadata.content_hash(),
        after.metadata.content_hash()
    );
}

#[test]
fn podcast_title_changes_the_metadata_and_feed_hash() {
    let retitled = FEED.replace("<title>Hash Test</title>", "<title>Hash Test 2</title>");
    let (before, after) = (parse(FEED), parse(&retitled));
    assert_ne!(
        before.metadata.content_hash(),
        after.metadata.content_hash()
    );
    assert_ne!(before.feed_hash(), after.feed_hash());
}

#[test]
fn item_order_does_not_change_the_feed_hash() {
    let first = &FEED[FEED.find("  <item>").expect("has items")
        ..FEED
            .find("  <item>\n    <title>Second")
            .expect("has a second item")];
    let reordered = FEED
        .replacen(first, "", 1)
        .replace("</channel>", &format!("{first}</channel>"));
    assert_eq!(parse(&reordered).episodes[0].guid, "ep-2");
    assert_eq!(feed_hash(&reordered), feed_hash(FEED));
}

#[test]
fn removing_an_episode_changes_the_feed_hash() {
    let start = FEED
        .find("  <item>\n    <title>Second")
        .expect("has a second item");
    let end = FEED.rfind("</channel>").expect("has a channel");
    let shorter = format!("{}{}", &FEED[..start], &FEED[end..]);
    assert_eq!(parse(&shorter).episodes.len(), 1);
    assert_ne!(feed_hash(&shorter), feed_hash(FEED));
}
