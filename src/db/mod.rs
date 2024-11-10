use crate::model::{
    Episode, EpisodeWithProgress, Podcast, PodcastWithEpisodeStats, Session, User, UserEpisode,
    UserSubscription,
};

type Result<T> = std::result::Result<T, sqlx::Error>;

pub struct Db {
    pool: sqlx::PgPool,
}

impl Db {
    pub async fn init(pool: sqlx::PgPool) -> Result<Self> {
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn find_podcast_by_feed_url(&self, feed_url: &str) -> Result<Option<Podcast>> {
        let podcast = sqlx::query_as!(
            Podcast,
            r#"
            SELECT * FROM podcast WHERE feed_url = $1
            "#,
            feed_url
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(podcast)
    }

    pub async fn list_podcasts(&self) -> Result<Vec<Podcast>> {
        let podcasts = sqlx::query_as!(Podcast, r#"SELECT * FROM podcast"#)
            .fetch_all(&self.pool)
            .await?;
        Ok(podcasts)
    }

    pub async fn insert_podcast(&self, podcast: &Podcast) -> Result<Podcast> {
        let tx = self.pool.begin().await?;
        let existing = self.find_podcast_by_feed_url(&podcast.feed_url).await?;
        let podcast = if let Some(existing) = existing {
            existing
        } else {
            let p = sqlx::query_as!(
                Podcast,
                r#"
                INSERT INTO podcast (id, title, description, image_link, feed_url, feed_type, created_at, last_updated)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                RETURNING *
                "#,
                podcast.id,
                podcast.title,
                podcast.description,
                podcast.image_link,
                podcast.feed_url,
                podcast.feed_type,
                podcast.created_at,
                podcast.last_updated,
            )
            .fetch_one(&self.pool)
            .await?;

            p
        };

        tx.commit().await?;
        Ok(podcast)
    }

    pub async fn find_episode_by_id(&self, id: &str) -> Result<Option<Episode>> {
        let episode = sqlx::query_as!(
            Episode,
            r#"
            SELECT * FROM episode WHERE id = $1
            "#,
            id
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(episode)
    }

    pub async fn insert_episode(&self, episode: Episode) -> Result<Episode> {
        let tx = self.pool.begin().await?;
        let existing = self.find_episode_by_id(&episode.id).await?;
        let episode = if let Some(existing) = existing {
            existing
        } else {
            sqlx::query!(
                r#"
                INSERT INTO episode (id, podcast_id, title, summary, summary_type, publication_date, audio_url, audio_type, audio_duration, thumbnail_url, created_at, last_updated)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                "#,

                episode.id,
                episode.podcast_id,
                episode.title,
                episode.summary,
                episode.summary_type,
                episode.publication_date,
                episode.audio_url,
                episode.audio_type,
                episode.audio_duration,
                episode.thumbnail_url,
                episode.created_at,
                episode.last_updated,
            ).execute(&self.pool).await?;

            episode
        };
        tx.commit().await?;
        Ok(episode)
    }

    pub async fn find_user_by_email(&self, email: &str) -> Result<Option<User>> {
        let user = sqlx::query_as!(
            User,
            r#"
            SELECT id, email, created_at, last_updated FROM users WHERE email = $1
            "#,
            email
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(user)
    }

    pub async fn find_user_by_session_id(&self, session_id: &str) -> Result<Option<User>> {
        let user = sqlx::query_as!(
            User,
            r#"
            SELECT u.id, u.email, u.created_at, u.last_updated
            FROM users u
            JOIN sessions s ON u.id = s.user_id
            WHERE s.session_id = $1
            "#,
            session_id
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(user)
    }

    pub async fn insert_user(&self, email: &str) -> Result<User> {
        let tx = self.pool.begin().await?;
        let existing = self.find_user_by_email(email).await?;
        let user = if let Some(existing) = existing {
            existing
        } else {
            let user = sqlx::query_as!(
                User,
                r#"
                INSERT INTO users (email, created_at, last_updated)
                VALUES ($1, $2, $3)
                RETURNING id, email, created_at, last_updated
                "#,
                email,
                chrono::Utc::now(),
                chrono::Utc::now(),
            )
            .fetch_one(&self.pool)
            .await?;

            user
        };
        tx.commit().await?;
        Ok(user)
    }

    pub async fn update_user_session(
        &self,
        user_id: uuid::Uuid,
        session_id: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Session> {
        let session = sqlx::query_as!(
            Session,
            r#"
            INSERT INTO sessions (user_id, session_id, expires_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (user_id) DO UPDATE SET session_id = $2, expires_at = $3
            RETURNING id, user_id, session_id, expires_at
            "#,
            user_id,
            session_id,
            expires_at,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(session)
    }

    pub async fn get_podcast_by_url(&self, url: &str) -> Result<Option<Podcast>> {
        let podcast = sqlx::query_as!(
            Podcast,
            r#"
            SELECT * FROM podcast WHERE feed_url = $1
            "#,
            url
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(podcast)
    }

    pub async fn add_subscription(
        &self,
        email: &str,
        podcast_id: &str,
    ) -> Result<UserSubscription> {
        let existing = sqlx::query_as!(
            UserSubscription,
            r#"
            SELECT * FROM user_subscription WHERE user_id = (SELECT id FROM users WHERE email = $1) AND podcast_id = $2
            "#,
            email,
            podcast_id
        ).fetch_optional(&self.pool).await?;

        if let Some(existing) = existing {
            Ok(existing)
        } else {
            let subscription = sqlx::query_as!(
                UserSubscription,
                r#"
                INSERT INTO user_subscription (user_id, podcast_id)
                VALUES ((SELECT id FROM users WHERE email = $1), $2)
                RETURNING id, user_id, podcast_id, created_at, last_updated
                "#,
                email,
                podcast_id
            )
            .fetch_one(&self.pool)
            .await?;
            Ok(subscription)
        }
    }

    pub async fn get_subscribed_podcasts_for_user(
        &self,
        email: &str,
    ) -> Result<Vec<PodcastWithEpisodeStats>> {
        let podcasts = sqlx::query_as!(
            PodcastWithEpisodeStats,
            r#"
            SELECT p.*, (SELECT MAX(e.publication_date) FROM episode e WHERE e.podcast_id = p.id) as last_publication_date FROM podcast p
            JOIN user_subscription us ON p.id = us.podcast_id
            JOIN users u ON us.user_id = u.id
            WHERE u.email = $1
            ORDER BY (SELECT MAX(e.publication_date) FROM episode e WHERE e.podcast_id = p.id) DESC
            "#,
            email
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(podcasts)
    }

    pub async fn get_podcast_by_id(&self, id: &str) -> Result<Option<Podcast>> {
        let podcast = sqlx::query_as!(
            Podcast,
            r#"
            SELECT * FROM podcast WHERE id = $1
            "#,
            id
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(podcast)
    }

    pub async fn get_episodes_for_podcast(&self, id: &str) -> Result<Vec<Episode>> {
        let episodes = sqlx::query_as!(
            Episode,
            r#"
            SELECT * FROM episode WHERE podcast_id = $1
            "#,
            id
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(episodes)
    }

    pub async fn get_episodes_with_progress_for_podcast(
        &self,
        email: &str,
        id: &str,
    ) -> Result<Vec<EpisodeWithProgress>> {
        let episodes: Vec<EpisodeWithProgress> = sqlx::query_as(
            r#"
                SELECT e.*, ue.progress
                FROM episode e
                LEFT JOIN user_episode ue ON e.id = ue.episode_id AND ue.user_id = (SELECT id FROM users WHERE email = $1)
                WHERE e.podcast_id = $2
            "#,
        )
        .bind(email)
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(episodes)
    }

    pub async fn update_progress(
        &self,
        email: &str,
        id: &str,
        progress: i32,
        done: bool,
    ) -> Result<Option<UserEpisode>> {
        let episode = sqlx::query_as!(
            UserEpisode,
            r#"
                INSERT INTO user_episode (user_id, episode_id, progress, done)
                VALUES ((SELECT id FROM users WHERE email = $1), $2, $3, $4)
                ON CONFLICT ON CONSTRAINT unique_user_episode DO UPDATE SET progress = $3, done = $4, last_updated = current_timestamp
                RETURNING *
            "#,
            email, id, progress, done
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(episode)
    }

    pub async fn find_new_episode_ids(
        &self,
        podcast_id: &str,
        episode_ids: &[String],
    ) -> Result<Vec<String>> {
        struct EpisodeId {
            id: Option<String>,
        }

        let new_episode_ids: Vec<String> = sqlx::query_as!(
            EpisodeId,
            // r#"
            //     SELECT id FROM (SELECT unnest($2::text[]) as id) as i WHERE i.id NOT IN (SELECT e.id FROM episode e WHERE e.podcast_id = $1)
            // "#,
            r#"
                SELECT i.id as id FROM (SELECT unnest($1::text[]) as id) as i WHERE i.id NOT IN (SELECT e.id FROM episode e)
            "#,
            // podcast_id,
            episode_ids
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .flat_map(|e| e.id)
        .collect();
        Ok(new_episode_ids)
    }
}
