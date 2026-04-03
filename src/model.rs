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

#[derive(serde::Serialize)]
pub struct Podcast {
    pub id: String,

    pub title: String,
    pub description: String,
    pub image_link: String,

    pub feed_url: String,
    pub feed_type: String,

    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Serialize)]
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
}

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct Episode {
    pub id: String,
    pub podcast_id: String,
    pub title: String,
    pub summary: String,
    pub summary_type: String,
    pub publication_date: chrono::DateTime<chrono::Utc>,
    pub audio_url: String,
    pub audio_type: String,
    pub audio_duration: i32,
    pub thumbnail_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct EpisodeWithProgress {
    #[sqlx(flatten)]
    pub episode: Episode,

    pub progress: Option<i32>,
    pub done: bool,
}

#[derive(serde::Serialize)]
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
#[derive(serde::Serialize)]
pub struct SyncResponse {
    pub server_time: chrono::DateTime<chrono::Utc>,
    pub next_since: String,
    pub has_more: bool,
    pub changes: Vec<SyncChange>,
}

/// A single change entry in the sync response.
#[derive(serde::Serialize)]
pub struct SyncChange {
    pub seq: i64,
    #[serde(rename = "type")]
    pub change_type: &'static str,
    pub op: &'static str,
    pub podcast_id: String,
    pub episode: Episode,
}
