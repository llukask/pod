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

/// A row from the episode_change table, joined with episode data for the
/// sync response.
#[derive(Debug)]
pub struct EpisodeChangeRow {
    pub seq: i64,
    pub podcast_id: String,
    pub episode_id: String,
    pub op: String,
}
