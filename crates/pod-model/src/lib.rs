#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Podcast {
    pub id: String,

    pub title: String,
    pub description: String,
    pub image_link: String,

    pub feed_url: String,
    pub feed_type: String,

    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,

    /// Cached ETag from the last successful RSS fetch, used for conditional
    /// HTTP requests to reduce feed-polling traffic.
    #[serde(skip, default)]
    pub feed_etag: Option<String>,

    /// Cached `Last-Modified` header value, used as a fallback for conditional
    /// requests when the feed server doesn't provide ETags.
    #[serde(skip, default)]
    pub feed_last_modified: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PodcastWithEpisodeStats {
    pub id: String,

    pub title: String,
    pub description: String,
    pub image_link: String,

    pub feed_url: String,
    pub feed_type: String,

    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,

    pub last_publication_date: Option<chrono::DateTime<chrono::Utc>>,

    #[serde(skip, default)]
    pub feed_etag: Option<String>,
    #[serde(skip, default)]
    pub feed_last_modified: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Episode {
    pub id: String,
    pub podcast_id: String,
    pub title: String,
    pub summary: String,
    pub summary_type: String,
    pub content_encoded: String,
    pub content_encoded_type: String,
    pub publication_date: chrono::DateTime<chrono::Utc>,
    pub audio_url: String,
    pub audio_type: String,
    pub audio_duration: i32,
    pub thumbnail_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct EpisodeWithProgress {
    #[cfg_attr(feature = "sqlx", sqlx(flatten))]
    pub episode: Episode,

    pub progress: Option<i32>,
    pub done: bool,
}

/// An episode with progress and parent podcast metadata, used for the
/// cross-podcast inbox view.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct InboxEpisode {
    #[cfg_attr(feature = "sqlx", sqlx(flatten))]
    pub episode: Episode,

    pub progress: Option<i32>,
    pub done: bool,

    pub podcast_title: String,
    pub podcast_image_link: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProgressState {
    pub progress: i32,
    pub done: bool,
}

// ==============================================================================
// Progress sync types
// ==============================================================================

/// A single progress entry returned by the progress sync endpoint.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProgressChange {
    pub episode_id: String,
    pub podcast_id: String,
    pub progress: i32,
    pub done: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Response for GET /api/v1/sync/progress.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProgressSyncResponse {
    pub server_time: chrono::DateTime<chrono::Utc>,
    pub changes: Vec<ProgressChange>,
}

// ==============================================================================
// Sync protocol types
// ==============================================================================

/// Full sync response returned by GET /api/v1/sync/changes.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SyncResponse {
    pub server_time: chrono::DateTime<chrono::Utc>,
    pub next_since: String,
    pub has_more: bool,
    pub changes: Vec<SyncChange>,
}

/// A single change entry in the sync response.
///
/// TODO: support `op: "delete"` changes. When implemented, `episode` should
/// become optional (present only for upserts) and an `episode_tombstone`
/// field should be added for deletes (containing `id` and `deleted_at`).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SyncChange {
    pub seq: i64,
    #[serde(rename = "type")]
    pub change_type: String,
    pub op: String,
    pub podcast_id: String,
    pub episode: Episode,
}
