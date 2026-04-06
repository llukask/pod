pub struct User {
    pub id: uuid::Uuid,
    pub username: String,
    pub password_hash: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

pub struct Session {
    pub id: uuid::Uuid,
    pub user_id: uuid::Uuid,
    pub session_id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Serialize, serde::Deserialize)]
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

#[derive(serde::Serialize, serde::Deserialize)]
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

#[derive(sqlx::FromRow, serde::Serialize, serde::Deserialize)]
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

#[derive(sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct EpisodeWithProgress {
    #[sqlx(flatten)]
    pub episode: Episode,

    pub progress: Option<i32>,
    pub done: bool,
}

/// An episode with progress and parent podcast metadata, used for the
/// cross-podcast inbox view.
#[derive(sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct InboxEpisode {
    #[sqlx(flatten)]
    pub episode: Episode,

    pub progress: Option<i32>,
    pub done: bool,

    pub podcast_title: String,
    pub podcast_image_link: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProgressState {
    pub progress: i32,
    pub done: bool,
}

pub struct UserSubscription {
    pub id: String,
    pub user_id: uuid::Uuid,
    pub podcast_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

pub struct UserEpisode {
    pub id: String,
    pub user_id: uuid::Uuid,
    pub episode_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,

    pub done: bool,
    pub progress: i32,
}

// ==============================================================================
// Progress sync types
// ==============================================================================

/// A single progress entry returned by the progress sync endpoint.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProgressChange {
    pub episode_id: String,
    pub podcast_id: String,
    pub progress: i32,
    pub done: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Response for GET /api/v1/sync/progress.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProgressSyncResponse {
    pub server_time: chrono::DateTime<chrono::Utc>,
    pub changes: Vec<ProgressChange>,
}

// ==============================================================================
// Sync protocol types
// ==============================================================================

/// A row from the episode_change table, joined with episode data for the
/// sync response.
#[derive(Debug)]
pub struct EpisodeChangeRow {
    pub seq: i64,
    pub podcast_id: String,
    pub episode_id: String,
    pub op: String,
}

/// Full sync response returned by GET /api/v1/sync/changes.
#[derive(serde::Serialize, serde::Deserialize)]
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
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SyncChange {
    pub seq: i64,
    #[serde(rename = "type")]
    pub change_type: String,
    pub op: String,
    pub podcast_id: String,
    pub episode: Episode,
}
