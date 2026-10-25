//! Bodies of the read endpoints that serve the online web client (spec 4.5).
//!
//! Every list item carries the entity it lists exactly as `POST /v1/sync`
//! sends it, flattened, plus the few fields the web client would otherwise
//! need a second request for. Episodes keep their `"deleted": false`, so one
//! episode parser serves both sync and the read endpoints.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

use crate::schema::{self, Variant};
use crate::{Episode, EpisodeState, Podcast, Timestamp};

/// Items per page when the request names no `limit`.
pub const DEFAULT_PAGE_LIMIT: u32 = 50;

/// Largest `limit` honoured; larger values are clamped to it, and `0` reads
/// as `1`.
pub const MAX_PAGE_LIMIT: u32 = 200;

/// The `limit` a paginated read endpoint uses for the requested one.
pub fn clamp_page_limit(requested: Option<u32>) -> u32 {
    requested
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT)
}

// ==============================================================================
// Podcasts
// ==============================================================================

/// The response to `GET /v1/podcasts`: every subscribed podcast in one page,
/// since a user's subscriptions are few enough to need no cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PodcastList {
    /// Newest `latest_published_at` first. Podcasts without episodes (a
    /// `pending` podcast the poller hasn't fetched yet, or an empty feed)
    /// come first, so a fresh subscription shows at the top while it loads.
    /// Ties break by `id` descending.
    pub items: Vec<PodcastItem>,
}

/// A subscribed podcast, as `GET /v1/podcasts` lists it and
/// `GET /v1/podcasts/{id}` returns it.
///
/// A `pending` podcast is included with `version` 0 and null metadata, so the
/// web client can show it as loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PodcastItem {
    #[serde(flatten)]
    pub podcast: Podcast,
    /// The newest `published_at` among the podcast's live episodes, or null
    /// when it has none.
    pub latest_published_at: Option<Timestamp>,
}

// ==============================================================================
// Episodes
// ==============================================================================

/// The response to `GET /v1/podcasts/{id}/episodes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodePage {
    /// Newest `published_at` first, ties by `id` descending. Deleted episodes
    /// are left out.
    pub items: Vec<EpisodeItem>,
    /// Pass as `cursor` to get the next page; null on the last page.
    pub next_cursor: Option<String>,
}

/// An episode with the user's progress on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeItem {
    #[serde(flatten)]
    pub episode: LiveEpisode,
    /// The user's row, or null when the user never touched the episode,
    /// which means position 0, not played and not archived.
    pub episode_state: Option<EpisodeState>,
}

// ==============================================================================
// Inbox
// ==============================================================================

/// The response to `GET /v1/inbox` (spec section 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InboxPage {
    /// Newest `published_at` first, ties by `id` descending.
    pub items: Vec<InboxItem>,
    /// Pass as `cursor` to get the next page; null on the last page.
    pub next_cursor: Option<String>,
}

/// An inbox episode with what an inbox row shows of its podcast.
///
/// Inbox episodes are unplayed and unarchived by definition, so the only
/// user state carried is the position, for resuming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InboxItem {
    #[serde(flatten)]
    pub episode: LiveEpisode,
    pub podcast_title: Option<String>,
    pub podcast_artwork_url: Option<String>,
    /// The user's position, 0 when they never started it.
    pub position_ms: i64,
}

// ==============================================================================
// Settings
// ==============================================================================

/// The response to `GET /v1/settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SettingsResponse {
    /// Every key the user stored, plus every known key (spec 2.4) they
    /// didn't, at its default. Values are any JSON. The web client changes one with a `setting.set`
    /// mutation through `POST /v1/sync`.
    #[schema(value_type = BTreeMap<String, Value>)]
    pub settings: BTreeMap<String, serde_json::Value>,
}

// ==============================================================================
// Live episodes
// ==============================================================================

/// An [`Episode`] on a read endpoint. It serializes with `"deleted": false`
/// and shares the `Episode` schema component with
/// [`ScopeEpisode`](crate::ScopeEpisode)'s live variant, because the read
/// endpoints never return a deleted episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveEpisode(pub Episode);

#[derive(Serialize)]
struct LiveEpisodeOut<'a> {
    #[serde(flatten)]
    episode: &'a Episode,
    deleted: bool,
}

#[derive(Deserialize)]
struct LiveEpisodeIn {
    #[serde(flatten)]
    episode: Episode,
    deleted: bool,
}

impl Serialize for LiveEpisode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        LiveEpisodeOut {
            episode: &self.0,
            deleted: false,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LiveEpisode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let LiveEpisodeIn { episode, deleted } = LiveEpisodeIn::deserialize(deserializer)?;
        if deleted {
            return Err(D::Error::custom(
                "a read endpoint never returns a deleted episode",
            ));
        }
        Ok(Self(episode))
    }
}

impl PartialSchema for LiveEpisode {
    fn schema() -> RefOr<Schema> {
        schema::tagged("deleted", Variant::of::<Episode>(false))
    }
}

impl ToSchema for LiveEpisode {
    /// The same component as the live variant of `ScopeEpisode`, which this
    /// schema equals.
    fn name() -> Cow<'static, str> {
        Episode::name()
    }

    fn schemas(out: &mut Vec<(String, RefOr<Schema>)>) {
        Episode::schemas(out);
    }
}
