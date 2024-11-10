pub struct User {
    pub id: uuid::Uuid,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

pub struct Session {
    pub id: uuid::Uuid,
    pub user_id: uuid::Uuid,
    pub session_id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

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

#[derive(sqlx::FromRow)]
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

#[derive(sqlx::FromRow)]
pub struct EpisodeWithProgress {
    #[sqlx(flatten)]
    pub episode: Episode,

    pub progress: Option<i32>,
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
