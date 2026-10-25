//! Change detection hashes (spec section 3.3).
//!
//! Each hash is SHA-256 over canonical JSON: an object with sorted keys and
//! no whitespace. Keys are sorted by building the object as a `BTreeMap`,
//! which serializes in key order whatever `serde_json` features another crate
//! enables (`preserve_order` would otherwise keep insertion order for
//! `serde_json::Map`).
//!
//! Only client-visible fields go in, so volatile feed fields such as
//! `lastBuildDate` never change a hash.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{ParsedEpisode, ParsedFeed, PodcastMetadata};

/// A SHA-256 digest, stored as `bytea` and compared by ingest.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn of_canonical_json(object: BTreeMap<&str, Value>) -> Self {
        let bytes = serde_json::to_vec(&object).expect("JSON values always serialize");
        Self(Sha256::digest(bytes).into())
    }

    fn to_hex(self) -> String {
        format!("{self:?}")
    }
}

/// Lowercase hex, so snapshots and logs stay readable.
impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl ParsedEpisode {
    /// The `episode.content_hash` column.
    ///
    /// `id` and `guid` are left out: the id derives from the guid, and the
    /// feed hash pairs each guid with this hash already.
    pub fn content_hash(&self) -> ContentHash {
        ContentHash::of_canonical_json(self.canonical_fields())
    }

    fn canonical_fields(&self) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("title", json!(self.title)),
            ("summary_html", json!(self.summary_html)),
            ("content_html", json!(self.content_html)),
            ("link", json!(self.link)),
            // The wire form: RFC 3339, UTC, exactly three fractional digits.
            ("published_at", json!(self.published_at)),
            ("duration_ms", json!(self.duration_ms)),
            ("audio_url", json!(self.audio_url)),
            ("audio_type", json!(self.audio_type)),
            ("audio_bytes", json!(self.audio_bytes)),
            ("artwork_url", json!(self.artwork_url)),
            ("season", json!(self.season)),
            ("number", json!(self.number)),
        ])
    }
}

impl PodcastMetadata {
    /// The `podcast.metadata_hash` column.
    pub fn content_hash(&self) -> ContentHash {
        ContentHash::of_canonical_json(BTreeMap::from([
            ("title", json!(self.title)),
            ("author", json!(self.author)),
            ("description", json!(self.description)),
            ("link", json!(self.link)),
            ("artwork_url", json!(self.artwork_url)),
            ("language", json!(self.language)),
        ]))
    }
}

impl ParsedFeed {
    /// The `feed_poll.content_hash` column: hashes
    /// `{"episodes":[[guid,episode_hash],...],"metadata":metadata_hash}` with
    /// the pairs sorted by guid and the hashes as lowercase hex.
    ///
    /// Sorting makes the hash independent of item order, so a publisher
    /// reordering its feed doesn't trigger an ingest. `new_feed_url` isn't
    /// part of it, so the poller has to act on that field before comparing
    /// hashes.
    pub fn feed_hash(&self) -> ContentHash {
        let mut episodes: Vec<(&str, String)> = self
            .episodes
            .iter()
            .map(|episode| (episode.guid.as_str(), episode.content_hash().to_hex()))
            .collect();
        episodes.sort_unstable();
        ContentHash::of_canonical_json(BTreeMap::from([
            ("metadata", json!(self.metadata.content_hash().to_hex())),
            ("episodes", json!(episodes)),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use pod_protocol::{EpisodeId, PodcastId, Timestamp};

    use super::*;

    fn episode() -> ParsedEpisode {
        let podcast_id: PodcastId = "9c3e0000-0000-5000-8000-000000000000"
            .parse()
            .expect("literal UUID");
        ParsedEpisode {
            id: EpisodeId::new(podcast_id, "ep-1"),
            guid: "ep-1".to_owned(),
            title: "Ünïcode \"quoted\"".to_owned(),
            summary_html: "<p>a</p>".to_owned(),
            content_html: String::new(),
            link: None,
            published_at: Timestamp::from_epoch_ms(1_760_936_400_120).expect("in range"),
            duration_ms: Some(3_612_000),
            audio_url: "https://cdn.example.com/1.mp3".to_owned(),
            audio_type: "audio/mpeg".to_owned(),
            audio_bytes: None,
            artwork_url: None,
            season: None,
            number: Some(212),
        }
    }

    #[test]
    fn episode_canonical_json_is_sorted_and_compact() {
        let bytes = serde_json::to_vec(&episode().canonical_fields()).expect("serializes");
        assert_eq!(
            String::from_utf8(bytes).expect("JSON is UTF-8"),
            concat!(
                r#"{"artwork_url":null,"audio_bytes":null,"audio_type":"audio/mpeg","#,
                r#""audio_url":"https://cdn.example.com/1.mp3","content_html":"","#,
                r#""duration_ms":3612000,"link":null,"number":212,"#,
                r#""published_at":"2025-10-20T05:00:00.120Z","season":null,"#,
                r#""summary_html":"<p>a</p>","title":"Ünïcode \"quoted\""}"#,
            )
        );
    }
}
