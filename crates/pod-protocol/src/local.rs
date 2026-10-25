//! Client-side rows of the local schema (spec 5.1) that sync touches, and
//! the merge of pulled rows into them (spec 5.4).
//!
//! These mirror the SQLite columns one to one, except that timestamps are
//! [`Timestamp`]s (SQLite holds epoch ms), booleans are `bool` (SQLite holds
//! 0/1) and a setting's value is parsed JSON (SQLite holds its text). They
//! serialize with the column names, which is the row shape the conformance
//! fixtures use. Columns with an SQL default get the same serde default.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::lww::{Stamp, wins};
use crate::{
    DeviceId, EpisodeId, EpisodeState, Mutation, MutationId, PodcastId, Setting, Subscription,
    Timestamp, UserId,
};

// ==============================================================================
// User state rows
// ==============================================================================

/// A row of the local `subscription` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSubscription {
    pub podcast_id: PodcastId,
    pub feed_url: String,
    pub subscribed: bool,
    pub subscribed_at: Timestamp,
    pub subscribed_dev: DeviceId,
    #[serde(default)]
    pub replaced_by: Option<PodcastId>,
}

/// A row of the local `episode_state` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalEpisodeState {
    pub episode_id: EpisodeId,
    pub podcast_id: PodcastId,
    #[serde(default)]
    pub position_ms: i64,
    #[serde(default)]
    pub position_at: Option<Timestamp>,
    #[serde(default)]
    pub position_dev: Option<DeviceId>,
    #[serde(default)]
    pub played: bool,
    #[serde(default)]
    pub played_at: Option<Timestamp>,
    #[serde(default)]
    pub played_dev: Option<DeviceId>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub archived_at: Option<Timestamp>,
    #[serde(default)]
    pub archived_dev: Option<DeviceId>,
}

/// A row of the local `setting` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSetting {
    pub key: String,
    pub value: serde_json::Value,
    pub value_at: Timestamp,
    pub value_dev: DeviceId,
}

// ==============================================================================
// Merge (spec 5.4)
// ==============================================================================

// Each LWW field is decided on its own: a pulled row can win `position` and
// lose `played` at once. Pulled rows always go through `merge`, never a plain
// overwrite, so a local edit still in the outbox (which carries a newer `at`
// than the server row) survives the pull. Local user actions don't merge;
// they overwrite (spec 5.2).

impl From<&Subscription> for LocalSubscription {
    /// The local row for a pulled subscription with no local row yet.
    fn from(remote: &Subscription) -> Self {
        Self {
            podcast_id: remote.podcast_id,
            feed_url: remote.feed_url.clone(),
            subscribed: remote.subscribed,
            subscribed_at: remote.subscribed_at,
            subscribed_dev: remote.subscribed_dev,
            replaced_by: remote.replaced_by,
        }
    }
}

impl LocalSubscription {
    /// Merges a pulled row for the same podcast into this one.
    ///
    /// `feed_url` has no stamp of its own. `subscription.set` writes it
    /// together with `subscribed`, so it moves with `subscribed`.
    ///
    /// `replaced_by` isn't an LWW field: the server sets it and the pulled
    /// row is the server's current word on it, so it is always taken from
    /// the remote row. Acting on it (moving the subscription to the
    /// canonical podcast) is the sync engine's job, whatever the merge
    /// decided (spec 5.5).
    pub fn merge(&mut self, remote: &Subscription) {
        let local = Stamp {
            at: self.subscribed_at,
            dev: self.subscribed_dev,
        };
        let pulled = Stamp {
            at: remote.subscribed_at,
            dev: remote.subscribed_dev,
        };
        if wins(pulled, Some(local)) {
            self.subscribed = remote.subscribed;
            self.feed_url.clone_from(&remote.feed_url);
            self.subscribed_at = remote.subscribed_at;
            self.subscribed_dev = remote.subscribed_dev;
        }
        self.replaced_by = remote.replaced_by;
    }
}

impl From<&EpisodeState> for LocalEpisodeState {
    /// The local row for a pulled episode state with no local row yet.
    fn from(remote: &EpisodeState) -> Self {
        Self {
            episode_id: remote.episode_id,
            podcast_id: remote.podcast_id,
            position_ms: remote.position_ms,
            position_at: remote.position_at,
            position_dev: remote.position_dev,
            played: remote.played,
            played_at: remote.played_at,
            played_dev: remote.played_dev,
            archived: remote.archived,
            archived_at: remote.archived_at,
            archived_dev: remote.archived_dev,
        }
    }
}

impl LocalEpisodeState {
    /// Merges a pulled row for the same episode into this one, field by
    /// field.
    ///
    /// A pulled field that was never written (null stamp) never replaces a
    /// local one: spec 4.4's `wins` has no case for a null new `at`, and the
    /// server only ever sends null for a field nobody wrote, whose value is
    /// the column default anyway.
    pub fn merge(&mut self, remote: &EpisodeState) {
        merge_nullable_field(
            (
                &mut self.position_ms,
                &mut self.position_at,
                &mut self.position_dev,
            ),
            (remote.position_ms, remote.position_at, remote.position_dev),
        );
        merge_nullable_field(
            (&mut self.played, &mut self.played_at, &mut self.played_dev),
            (remote.played, remote.played_at, remote.played_dev),
        );
        merge_nullable_field(
            (
                &mut self.archived,
                &mut self.archived_at,
                &mut self.archived_dev,
            ),
            (remote.archived, remote.archived_at, remote.archived_dev),
        );
    }
}

/// Merges one `(value, *_at, *_dev)` field whose stamp columns are nullable.
fn merge_nullable_field<T>(
    (value, at, dev): (&mut T, &mut Option<Timestamp>, &mut Option<DeviceId>),
    (remote_value, remote_at, remote_dev): (T, Option<Timestamp>, Option<DeviceId>),
) {
    let Some(pulled) = Stamp::from_columns(remote_at, remote_dev) else {
        return;
    };
    if wins(pulled, Stamp::from_columns(*at, *dev)) {
        *value = remote_value;
        *at = Some(pulled.at);
        *dev = Some(pulled.dev);
    }
}

impl From<&Setting> for LocalSetting {
    /// The local row for a pulled setting with no local row yet.
    fn from(remote: &Setting) -> Self {
        Self {
            key: remote.key.clone(),
            value: remote.value.clone(),
            value_at: remote.value_at,
            value_dev: remote.value_dev,
        }
    }
}

impl LocalSetting {
    /// Merges a pulled row for the same key into this one.
    pub fn merge(&mut self, remote: &Setting) {
        let local = Stamp {
            at: self.value_at,
            dev: self.value_dev,
        };
        let pulled = Stamp {
            at: remote.value_at,
            dev: remote.value_dev,
        };
        if wins(pulled, Some(local)) {
            self.value.clone_from(&remote.value);
            self.value_at = remote.value_at;
            self.value_dev = remote.value_dev;
        }
    }
}

// ==============================================================================
// Sync bookkeeping rows
// ==============================================================================

/// The `scope` key of a `sync_scope` row: `user` or `podcast:<id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeKey {
    User,
    Podcast(PodcastId),
}

impl fmt::Display for ScopeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Podcast(id) => write!(f, "podcast:{id}"),
        }
    }
}

/// A `sync_scope` key that is neither `user` nor `podcast:<uuid>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid sync scope key: {0:?}")]
pub struct InvalidScopeKey(pub String);

impl FromStr for ScopeKey {
    type Err = InvalidScopeKey;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "user" {
            return Ok(Self::User);
        }
        s.strip_prefix("podcast:")
            .and_then(|id| id.parse().ok())
            .map(Self::Podcast)
            .ok_or_else(|| InvalidScopeKey(s.to_owned()))
    }
}

impl Serialize for ScopeKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ScopeKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A row of the local `sync_scope` table: the client's cursor in one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncScopeRow {
    pub scope: ScopeKey,
    pub version: i64,
    #[serde(default)]
    pub complete: bool,
}

/// A row of the local `outbox` table: a mutation not yet acknowledged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxRow {
    pub mutation_id: MutationId,
    /// Groups mutations that replace each other, e.g. `position:<episode_id>`
    /// (spec 5.2).
    pub coalesce_key: String,
    pub payload: Mutation,
    pub created_at: Timestamp,
    #[serde(default)]
    pub attempts: u32,
}

/// The single row of the local `account` table.
///
/// `device_id` is the current session's. Outbox mutations written before a
/// re-login still carry the previous session's id in their `dev` (spec 5.7).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAccount {
    pub server_url: String,
    pub user_id: UserId,
    pub username: String,
    pub device_id: DeviceId,
    pub token: String,
}

impl fmt::Debug for LocalAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalAccount")
            .field("server_url", &self.server_url)
            .field("user_id", &self.user_id)
            .field("username", &self.username)
            .field("device_id", &self.device_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_keys_round_trip_through_their_column_text() {
        let podcast: PodcastId = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c"
            .parse()
            .expect("literal is a UUID");
        for key in [ScopeKey::User, ScopeKey::Podcast(podcast)] {
            assert_eq!(key.to_string().parse(), Ok(key));
        }
        assert_eq!(
            ScopeKey::Podcast(podcast).to_string(),
            "podcast:9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c"
        );
        assert!("podcast:nope".parse::<ScopeKey>().is_err());
        assert!("users".parse::<ScopeKey>().is_err());
    }

    #[test]
    fn merge_takes_the_servers_replaced_by_even_when_the_local_stamp_wins() {
        let x: PodcastId = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c"
            .parse()
            .expect("literal is a UUID");
        let y: PodcastId = "a71b2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c3d"
            .parse()
            .expect("literal is a UUID");
        let dev: DeviceId = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d"
            .parse()
            .expect("literal is a UUID");
        let mut local = LocalSubscription {
            podcast_id: x,
            feed_url: "https://example.com/old.xml".into(),
            subscribed: true,
            subscribed_at: "2026-10-24T10:00:00Z".parse().expect("RFC 3339"),
            subscribed_dev: dev,
            replaced_by: None,
        };
        let remote = Subscription {
            podcast_id: x,
            feed_url: "https://example.com/old.xml".into(),
            subscribed: false,
            subscribed_at: "2026-10-24T09:00:00Z".parse().expect("RFC 3339"),
            subscribed_dev: dev,
            replaced_by: Some(y),
            version: 12,
        };
        local.merge(&remote);
        assert!(local.subscribed, "the newer local subscribe stands");
        assert_eq!(local.replaced_by, Some(y));
    }
}
