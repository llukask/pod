//! Wire types shared by the Pod server and its Rust clients.
//!
//! This crate stays free of I/O dependencies so every client can embed it
//! (spec section 11).

mod auth;
pub mod conformance;
mod directory;
mod entities;
mod error;
mod feed_url;
mod ids;
pub mod local;
mod lww;
mod mutation;
pub mod openapi;
mod schema;
pub mod settings;
mod sync;
mod timestamp;

pub use auth::{AuthRequest, MeResponse, TokenResponse};
pub use directory::{
    DEFAULT_DIRECTORY_LIMIT, DirectoryResult, DirectorySearchResponse, MAX_DIRECTORY_LIMIT,
};
pub use entities::{
    Episode, EpisodeState, EpisodeTombstone, Podcast, PodcastStatus, ScopeEpisode, Setting,
    Subscription,
};
pub use error::{ErrorBody, ErrorCode, ErrorDetail};
pub use feed_url::{FeedUrlError, NormalizedFeedUrl, normalize_feed_url};
pub use ids::{DeviceId, EpisodeId, MutationId, PodcastId, UserId, episode_key};
pub use lww::{MAX_CLOCK_LEAD, Stamp, clamp_mutation_at, wins};
pub use mutation::{
    EpisodeArchived, EpisodePlayed, EpisodePosition, MissingMutationId, Mutation,
    MutationRejection, SettingSet, SubscriptionSet,
};
pub use settings::{KNOWN_SETTINGS, KnownSetting, SettingDefault, setting_default};
pub use sync::{
    DEFAULT_SYNC_LIMIT, MAX_MUTATIONS, MAX_SYNC_LIMIT, MutationOutcome, MutationResult,
    NotSubscribedScope, PodcastChanges, PodcastCursor, PodcastScope, RejectionCode, ScopeChanges,
    ScopeCursors, SyncRequest, SyncResponse, UserScope,
};
pub use timestamp::{ParseTimestampError, Timestamp, TimestampOutOfRange};
