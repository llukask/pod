use crate::model::{
    Episode, EpisodeChangeRow, EpisodeWithProgress, Podcast, PodcastWithEpisodeStats, Session,
    User, UserEpisode, UserSubscription,
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
        // Use a no-op DO UPDATE so RETURNING always yields the row,
        // whether it was freshly inserted or already existed.
        let podcast = sqlx::query_as!(
            Podcast,
            r#"
            INSERT INTO podcast (id, title, description, image_link, feed_url, feed_type, created_at, last_updated)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (feed_url) DO UPDATE SET feed_url = EXCLUDED.feed_url
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

    /// Insert an episode and record an upsert entry in the change log,
    /// both within the same transaction so the sync stream stays consistent.
    pub async fn insert_episode(&self, episode: Episode) -> Result<Episode> {
        let mut tx = self.pool.begin().await?;

        let episode = sqlx::query_as!(
            Episode,
            r#"
            INSERT INTO episode (id, podcast_id, title, summary, summary_type, publication_date, audio_url, audio_type, audio_duration, thumbnail_url, created_at, last_updated)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (id) DO UPDATE SET id = EXCLUDED.id
            RETURNING *
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
        )
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query!(
            r#"
            INSERT INTO episode_change (podcast_id, episode_id, op)
            VALUES ($1, $2, 'upsert')
            "#,
            episode.podcast_id,
            episode.id,
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(episode)
    }

    pub async fn find_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let user = sqlx::query_as!(
            User,
            r#"
            SELECT id, username, password_hash, created_at, last_updated FROM users WHERE username = $1
            "#,
            username
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(user)
    }

    pub async fn find_user_by_session_id(&self, session_id: &str) -> Result<Option<User>> {
        let user = sqlx::query_as!(
            User,
            r#"
            SELECT u.id, u.username, u.password_hash, u.created_at, u.last_updated
            FROM users u
            JOIN sessions s ON u.id = s.user_id
            WHERE s.session_id = $1 AND s.expires_at > current_timestamp
            "#,
            session_id
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(user)
    }

    pub async fn insert_user(&self, username: &str, password_hash: &str) -> Result<User> {
        let user = sqlx::query_as!(
            User,
            r#"
            INSERT INTO users (username, password_hash, created_at, last_updated)
            VALUES ($1, $2, $3, $4)
            RETURNING id, username, password_hash, created_at, last_updated
            "#,
            username,
            password_hash,
            chrono::Utc::now(),
            chrono::Utc::now(),
        )
        .fetch_one(&self.pool)
        .await?;
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
        username: &str,
        podcast_id: &str,
    ) -> Result<UserSubscription> {
        let existing = sqlx::query_as!(
            UserSubscription,
            r#"
            SELECT * FROM user_subscription WHERE user_id = (SELECT id FROM users WHERE username = $1) AND podcast_id = $2
            "#,
            username,
            podcast_id
        ).fetch_optional(&self.pool).await?;

        if let Some(existing) = existing {
            Ok(existing)
        } else {
            let subscription = sqlx::query_as!(
                UserSubscription,
                r#"
                INSERT INTO user_subscription (user_id, podcast_id)
                VALUES ((SELECT id FROM users WHERE username = $1), $2)
                RETURNING id, user_id, podcast_id, created_at, last_updated
                "#,
                username,
                podcast_id
            )
            .fetch_one(&self.pool)
            .await?;
            Ok(subscription)
        }
    }

    pub async fn get_subscribed_podcasts_for_user(
        &self,
        username: &str,
    ) -> Result<Vec<PodcastWithEpisodeStats>> {
        let podcasts = sqlx::query_as!(
            PodcastWithEpisodeStats,
            r#"
            SELECT p.*, (SELECT MAX(e.publication_date) FROM episode e WHERE e.podcast_id = p.id) as last_publication_date FROM podcast p
            JOIN user_subscription us ON p.id = us.podcast_id
            JOIN users u ON us.user_id = u.id
            WHERE u.username = $1
            ORDER BY (SELECT MAX(e.publication_date) FROM episode e WHERE e.podcast_id = p.id) DESC
            "#,
            username
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(podcasts)
    }

    pub async fn update_podcast_cache_headers(
        &self,
        id: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<()> {
        sqlx::query!(
            r#"UPDATE podcast SET feed_etag = $2, feed_last_modified = $3 WHERE id = $1"#,
            id,
            etag,
            last_modified,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
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

    pub async fn get_podcast_for_user(
        &self,
        username: &str,
        podcast_id: &str,
    ) -> Result<Option<Podcast>> {
        let podcast = sqlx::query_as!(
            Podcast,
            r#"
            SELECT p.* FROM podcast p
            JOIN user_subscription us ON p.id = us.podcast_id
            JOIN users u ON us.user_id = u.id
            WHERE p.id = $1 AND u.username = $2
            "#,
            podcast_id,
            username
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
        username: &str,
        id: &str,
        pagination: Option<(i64, Option<(chrono::DateTime<chrono::Utc>, String)>)>,
    ) -> Result<Vec<EpisodeWithProgress>> {
        let episodes: Vec<EpisodeWithProgress> = if let Some((limit, cursor)) = pagination {
            let (cursor_date, cursor_id) = match cursor {
                Some((date, id)) => (Some(date), Some(id)),
                None => (None, None),
            };
            // Use a compound cursor (publication_date, id) so that episodes
            // sharing the same publication_date are not skipped.
            sqlx::query_as(
                r#"
                    SELECT e.*, ue.progress
                    , COALESCE(ue.done, false) AS done
                    FROM episode e
                    LEFT JOIN user_episode ue ON e.id = ue.episode_id AND ue.user_id = (SELECT id FROM users WHERE username = $1)
                    WHERE e.podcast_id = $2
                      AND (
                        $3::timestamptz IS NULL
                        OR e.publication_date < $3
                        OR (e.publication_date = $3 AND e.id < $4)
                      )
                    ORDER BY e.publication_date DESC, e.id DESC
                    LIMIT $5
                "#,
            )
            .bind(username)
            .bind(id)
            .bind(cursor_date)
            .bind(cursor_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                r#"
                    SELECT e.*, ue.progress
                    , COALESCE(ue.done, false) AS done
                    FROM episode e
                    LEFT JOIN user_episode ue ON e.id = ue.episode_id AND ue.user_id = (SELECT id FROM users WHERE username = $1)
                    WHERE e.podcast_id = $2
                    ORDER BY e.publication_date DESC, e.id DESC
                "#,
            )
            .bind(username)
            .bind(id)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(episodes)
    }

    pub async fn update_progress(
        &self,
        username: &str,
        id: &str,
        progress: i32,
        done: bool,
    ) -> Result<Option<UserEpisode>> {
        let episode = sqlx::query_as!(
            UserEpisode,
            r#"
                INSERT INTO user_episode (user_id, episode_id, progress, done)
                VALUES ((SELECT id FROM users WHERE username = $1), $2, $3, $4)
                ON CONFLICT ON CONSTRAINT unique_user_episode DO UPDATE SET progress = $3, done = $4, last_updated = current_timestamp
                RETURNING *
            "#,
            username, id, progress, done
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(episode)
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        sqlx::query!(r#"DELETE FROM sessions WHERE session_id = $1"#, session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
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
            r#"
                SELECT i.id as id FROM (SELECT unnest($1::text[]) as id) as i WHERE i.id NOT IN (SELECT e.id FROM episode e WHERE e.podcast_id = $2)
            "#,
            episode_ids,
            podcast_id
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .flat_map(|e| e.id)
        .collect();
        Ok(new_episode_ids)
    }

    // ==========================================================================
    // Sync protocol
    // ==========================================================================

    /// Return episode changes for a user's subscriptions since the given
    /// sequence number, ordered by seq ascending.  Fetches `limit + 1` rows
    /// so the caller can detect whether more pages remain.
    pub async fn get_sync_changes(
        &self,
        username: &str,
        since_seq: i64,
        limit: i64,
    ) -> Result<Vec<EpisodeChangeRow>> {
        let rows = sqlx::query_as!(
            EpisodeChangeRow,
            r#"
            SELECT ec.seq, ec.podcast_id, ec.episode_id, ec.op
            FROM episode_change ec
            JOIN user_subscription us
              ON us.podcast_id = ec.podcast_id
            JOIN users u
              ON u.id = us.user_id
            WHERE u.username = $1
              AND ec.seq > $2
            ORDER BY ec.seq ASC
            LIMIT $3
            "#,
            username,
            since_seq,
            limit + 1,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Return the latest seq visible to this user (for ETag generation).
    pub async fn get_latest_seq_for_user(&self, username: &str) -> Result<Option<i64>> {
        struct SeqRow {
            seq: Option<i64>,
        }
        let row = sqlx::query_as!(
            SeqRow,
            r#"
            SELECT MAX(ec.seq) as seq
            FROM episode_change ec
            JOIN user_subscription us
              ON us.podcast_id = ec.podcast_id
            JOIN users u
              ON u.id = us.user_id
            WHERE u.username = $1
            "#,
            username,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.seq)
    }
}
