//! Runs `conformance/ids.json`, the table the Android client also runs.

use std::path::Path;

use pod_protocol::{EpisodeId, FeedUrlError, PodcastId, episode_key, normalize_feed_url};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    namespaces: Namespaces,
    feed_urls: Vec<FeedUrlCase>,
    rejected_feed_urls: Vec<RejectedCase>,
    episode_keys: Vec<EpisodeKeyCase>,
    episode_ids: Vec<EpisodeIdCase>,
}

#[derive(Debug, Deserialize)]
struct Namespaces {
    podcast: String,
    episode: String,
}

#[derive(Debug, Deserialize)]
struct FeedUrlCase {
    name: String,
    input: String,
    feed_url: String,
    normalized: String,
    podcast_id: String,
}

#[derive(Debug, Deserialize)]
struct RejectedCase {
    name: String,
    input: String,
    error: String,
}

#[derive(Debug, Deserialize)]
struct EpisodeKeyCase {
    name: String,
    guid: Option<String>,
    enclosure_url: String,
    key: String,
}

#[derive(Debug, Deserialize)]
struct EpisodeIdCase {
    name: String,
    podcast_id: String,
    key: String,
    episode_id: String,
}

fn fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/ids.json");
    let text = std::fs::read_to_string(&path).expect("conformance/ids.json is checked in");
    serde_json::from_str(&text).expect("conformance/ids.json matches the fixture schema")
}

/// The error kinds the fixture names. Kotlin maps its own error type onto
/// the same strings.
fn error_kind(error: &FeedUrlError) -> &'static str {
    match error {
        FeedUrlError::Invalid(_) => "invalid_url",
        FeedUrlError::UnsupportedScheme(_) => "unsupported_scheme",
    }
}

#[test]
fn namespaces_match_the_crate_constants() {
    let fixture = fixture();
    assert_eq!(fixture.namespaces.podcast, PodcastId::NAMESPACE.to_string());
    assert_eq!(fixture.namespaces.episode, EpisodeId::NAMESPACE.to_string());
}

#[test]
fn feed_urls_normalize_to_the_expected_ids() {
    let mut failures = Vec::new();
    for case in fixture().feed_urls {
        let actual = match normalize_feed_url(&case.input) {
            Ok(url) => (
                url.feed_url().to_owned(),
                url.normalized().to_owned(),
                PodcastId::from_feed_url(&url).to_string(),
            ),
            Err(error) => {
                failures.push(format!("{}: rejected with {error}", case.name));
                continue;
            }
        };
        let expected = (case.feed_url, case.normalized, case.podcast_id);
        if actual != expected {
            failures.push(format!(
                "{}: expected {expected:?}, got {actual:?}",
                case.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn rejected_feed_urls_fail_with_the_expected_kind() {
    let mut failures = Vec::new();
    for case in fixture().rejected_feed_urls {
        match normalize_feed_url(&case.input) {
            Ok(url) => failures.push(format!("{}: accepted as {url:?}", case.name)),
            Err(error) if error_kind(&error) != case.error => failures.push(format!(
                "{}: expected {}, got {error:?}",
                case.name, case.error
            )),
            Err(_) => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn episode_keys_follow_the_guid_then_enclosure_rule() {
    for case in fixture().episode_keys {
        assert_eq!(
            episode_key(case.guid.as_deref(), &case.enclosure_url),
            case.key,
            "{}",
            case.name
        );
    }
}

#[test]
fn episode_ids_match_the_expected_ids() {
    for case in fixture().episode_ids {
        let podcast_id: PodcastId = case.podcast_id.parse().expect("fixture ids are valid");
        assert_eq!(
            EpisodeId::new(podcast_id, &case.key).to_string(),
            case.episode_id,
            "{}",
            case.name
        );
    }
}
