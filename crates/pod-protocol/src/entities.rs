//! Entity shapes pulled through `POST /v1/sync` (spec section 4.2).
//!
//! Nullable fields serialize as `null` rather than being omitted, matching
//! the spec's examples, and a missing nullable field reads as `None`.
//! Versions are `bigint` on the server, so they are `i64` here.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;

use crate::schema::{self, Variant};
use crate::{DeviceId, EpisodeId, PodcastId, Timestamp};

/// The four names a podcast's feed status goes by: the `status` tag of
/// [`FeedStatus`], and the text the server's and the clients' `podcast.status`
/// columns hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PodcastStatus {
    Pending,
    Ok,
    Failing,
    Gone,
}

/// A `podcast.status` value that isn't one of the four statuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown podcast status {0:?}")]
pub struct UnknownPodcastStatus(pub String);

/// Parses the wire (and database) spelling, for stores that keep the status
/// as text.
impl std::str::FromStr for PodcastStatus {
    type Err = UnknownPodcastStatus;

    fn from_str(status: &str) -> Result<Self, Self::Err> {
        match status {
            "pending" => Ok(Self::Pending),
            "ok" => Ok(Self::Ok),
            "failing" => Ok(Self::Failing),
            "gone" => Ok(Self::Gone),
            other => Err(UnknownPodcastStatus(other.to_owned())),
        }
    }
}

impl PodcastStatus {
    /// The wire and column spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ok => "ok",
            Self::Failing => "failing",
            Self::Gone => "gone",
        }
    }
}

/// Where a podcast's feed stands with the poller (spec 3.2), flattened into
/// [`Podcast`] and tagged by `status` on the wire. Only a failing feed
/// carries more than the tag, so the shape can't say "ok" and name an
/// error at the same time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FeedStatus {
    /// Subscribed but not fetched successfully yet.
    Pending,
    Ok,
    /// Fetches keep failing and the poller keeps trying. `failing_since` is
    /// when the current run of errors began, which for a podcast that was
    /// `ok` lies seven days before it turned failing.
    Failing {
        failing_since: Timestamp,
        error: FeedError,
    },
    /// The feed answered HTTP 410. Polling stopped; episodes stay.
    Gone,
}

impl FeedStatus {
    pub fn kind(&self) -> PodcastStatus {
        match self {
            Self::Pending => PodcastStatus::Pending,
            Self::Ok => PodcastStatus::Ok,
            Self::Failing { .. } => PodcastStatus::Failing,
            Self::Gone => PodcastStatus::Gone,
        }
    }
}

/// Why the poller's last fetch of a feed failed (spec 3.2): the closed set
/// of outcomes the server tells apart, tagged by `code`. Clients word it
/// with [`message`](Self::message) or their own translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum FeedError {
    /// An HTTP status other than success, 304 or 410.
    Http { status: u16 },
    /// The body didn't parse as a feed.
    NotAFeed,
    InvalidUrl,
    /// The URL's port is outside what the server fetches from.
    PortNotAllowed,
    /// The host resolves to an address the server won't fetch from.
    AddressNotAllowed,
    HostNotFound,
    TooManyRedirects,
    InvalidRedirect,
    /// The body passed the server's size limit.
    TooLarge,
    Timeout,
    /// No response at all.
    ConnectionFailed,
}

impl FeedError {
    /// One short sentence for the user, without a trailing period and never
    /// with the URL in it (spec 3.2).
    pub fn message(self) -> String {
        match self {
            Self::Http { status } => return format!("The feed returned HTTP {status}"),
            Self::NotAFeed => "The URL doesn't point to a podcast feed",
            Self::InvalidUrl => "The feed URL is invalid",
            Self::PortNotAllowed => "The feed's port is not allowed",
            Self::AddressNotAllowed => "The feed's address is not allowed",
            Self::HostNotFound => "The feed's host can't be found",
            Self::TooManyRedirects => "The feed redirects too many times",
            Self::InvalidRedirect => "The feed redirects somewhere invalid",
            Self::TooLarge => "The feed is too large",
            Self::Timeout => "The feed timed out",
            Self::ConnectionFailed => "The feed's server can't be reached",
        }
        .to_owned()
    }
}

/// The catalog row of a podcast scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Podcast {
    pub id: PodcastId,
    /// The URL the poller fetches. It can change when the feed moves; the id
    /// doesn't.
    pub feed_url: String,
    #[serde(flatten)]
    pub status: FeedStatus,
    pub title: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub link: Option<String>,
    pub artwork_url: Option<String>,
    pub language: Option<String>,
    pub version: i64,
}

/// A live episode in a podcast scope. On the wire it always carries
/// `"deleted": false`.
// `ScopeEpisode` adds `deleted`; this struct has no such field so it can't
// claim to be deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Episode {
    pub id: EpisodeId,
    pub podcast_id: PodcastId,
    pub title: String,
    /// Sanitized HTML from the feed's `description` (spec 2.2).
    pub summary_html: String,
    /// Sanitized HTML from the feed's `content:encoded`.
    pub content_html: String,
    pub link: Option<String>,
    pub published_at: Timestamp,
    pub duration_ms: Option<i64>,
    pub audio_url: String,
    /// The enclosure's MIME type.
    pub audio_type: String,
    pub audio_bytes: Option<i64>,
    pub artwork_url: Option<String>,
    pub season: Option<i32>,
    pub number: Option<i32>,
    pub version: i64,
}

/// A deleted episode (spec 4.2 rule 6). Clients remove it locally. On the
/// wire it carries `"deleted": true`.
// `ScopeEpisode` adds `deleted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeTombstone {
    pub id: EpisodeId,
    pub version: i64,
}

/// An entry in a podcast scope's `episodes`: a live episode or a tombstone,
/// told apart by the boolean `deleted`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "live episodes are the common case, boxing them would add an allocation per row"
)]
pub enum ScopeEpisode {
    Live(Episode),
    Deleted(EpisodeTombstone),
}

impl ScopeEpisode {
    pub fn id(&self) -> EpisodeId {
        match self {
            Self::Live(episode) => episode.id,
            Self::Deleted(tombstone) => tombstone.id,
        }
    }

    pub fn version(&self) -> i64 {
        match self {
            Self::Live(episode) => episode.version,
            Self::Deleted(tombstone) => tombstone.version,
        }
    }
}

#[derive(Serialize)]
struct WithDeleted<'a, T> {
    #[serde(flatten)]
    entity: &'a T,
    deleted: bool,
}

impl Serialize for ScopeEpisode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Live(entity) => WithDeleted {
                entity,
                deleted: false,
            }
            .serialize(serializer),
            Self::Deleted(entity) => WithDeleted {
                entity,
                deleted: true,
            }
            .serialize(serializer),
        }
    }
}

/// serde can't tag a union by a boolean, so this reads `deleted` first and
/// then parses the matching shape, which keeps the error messages specific.
impl<'de> Deserialize<'de> for ScopeEpisode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value.get("deleted") {
            Some(serde_json::Value::Bool(false)) => Episode::deserialize(value)
                .map(Self::Live)
                .map_err(D::Error::custom),
            Some(serde_json::Value::Bool(true)) => EpisodeTombstone::deserialize(value)
                .map(Self::Deleted)
                .map_err(D::Error::custom),
            _ => Err(D::Error::custom("episode needs a boolean `deleted` field")),
        }
    }
}

fn scope_episode_variants() -> Vec<Variant> {
    vec![
        Variant::of::<Episode>(false),
        Variant::of::<EpisodeTombstone>(true),
    ]
}

impl utoipa::PartialSchema for ScopeEpisode {
    fn schema() -> RefOr<Schema> {
        schema::one_of("deleted", &scope_episode_variants())
    }
}

impl ToSchema for ScopeEpisode {
    fn schemas(out: &mut Vec<(String, RefOr<Schema>)>) {
        schema::register("deleted", scope_episode_variants(), out);
    }
}

/// A user's subscription to a podcast (user scope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Subscription {
    pub podcast_id: PodcastId,
    pub feed_url: String,
    pub subscribed: bool,
    pub subscribed_at: Timestamp,
    pub subscribed_dev: DeviceId,
    /// Set by the server when this podcast id turned out to be an alias of
    /// another podcast (spec 4.4 "Alias redirect"). Not an LWW field.
    pub replaced_by: Option<PodcastId>,
    pub version: i64,
}

/// A user's progress on one episode (user scope). Each of the three fields
/// merges on its own `*_at` / `*_dev` pair (spec 5.4); a pair is `null`
/// until the field is first written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeState {
    pub episode_id: EpisodeId,
    pub podcast_id: PodcastId,
    pub position_ms: i64,
    pub position_at: Option<Timestamp>,
    pub position_dev: Option<DeviceId>,
    pub played: bool,
    pub played_at: Option<Timestamp>,
    pub played_dev: Option<DeviceId>,
    pub archived: bool,
    pub archived_at: Option<Timestamp>,
    pub archived_dev: Option<DeviceId>,
    pub version: i64,
}

/// One synced setting (user scope).
// `crate::KNOWN_SETTINGS` lists the keys clients understand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Setting {
    pub key: String,
    /// Any JSON value. Unknown keys are stored and synced unchanged.
    #[schema(value_type = Value)]
    pub value: serde_json::Value,
    pub value_at: Timestamp,
    pub value_dev: DeviceId,
    pub version: i64,
}
