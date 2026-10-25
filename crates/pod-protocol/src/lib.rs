//! Wire types shared by the Pod server and its Rust clients.
//!
//! This crate stays free of I/O dependencies so every client can embed it
//! (spec section 11).

mod auth;
mod entities;
mod error;
mod feed_url;
mod ids;
mod mutation;
pub mod openapi;
mod schema;
pub mod settings;
mod sync;
mod timestamp;

pub use auth::{AuthRequest, MeResponse, TokenResponse};
pub use entities::{
    Episode, EpisodeState, EpisodeTombstone, Podcast, PodcastStatus, ScopeEpisode, Setting,
    Subscription,
};
pub use error::{ErrorBody, ErrorCode, ErrorDetail};
pub use feed_url::{FeedUrlError, NormalizedFeedUrl, normalize_feed_url};
pub use ids::{DeviceId, EpisodeId, MutationId, PodcastId, UserId, episode_key};
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

// TODO: add the merge function (spec section 5.4) once a client needs it.
