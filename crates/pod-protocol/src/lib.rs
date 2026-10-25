//! Wire types shared by the Pod server and its Rust clients.
//!
//! This crate stays free of I/O dependencies so every client can embed it
//! (spec section 11).

mod feed_url;
mod ids;

pub use feed_url::{FeedUrlError, NormalizedFeedUrl, normalize_feed_url};
pub use ids::{DeviceId, EpisodeId, MutationId, PodcastId, episode_key};

// TODO: add the sync wire types and merge function (spec sections 4 and 5.4)
// once the server core needs them.
