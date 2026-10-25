//! `GET /v1/directory/search` bodies (spec 4.6).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::PodcastId;

/// Results per search when the request names no `limit`.
pub const DEFAULT_DIRECTORY_LIMIT: u32 = 25;

/// Largest `limit` honoured; larger values are clamped to it. The upstream
/// directory (iTunes) allows 200, but a search screen never needs that many.
pub const MAX_DIRECTORY_LIMIT: u32 = 50;

/// The response to `GET /v1/directory/search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectorySearchResponse {
    /// In the directory's relevance order.
    pub results: Vec<DirectoryResult>,
}

/// One podcast found in the directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectoryResult {
    pub title: String,
    pub author: Option<String>,
    /// The feed URL, normalized (spec section 1) with its original scheme.
    pub feed_url: String,
    pub artwork_url: Option<String>,
    /// The canonical id: the existing podcast if `feed_url` is a known alias
    /// of one, otherwise the id derived from `feed_url`. Clients compare it
    /// with their subscriptions to mark results they already follow.
    pub podcast_id: PodcastId,
}
