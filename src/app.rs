use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::error;

use crate::{
    db::Db,
    feed::entry_to_episode,
    http::errors::AppError,
    model::{EpisodeWithProgress, Podcast, PodcastWithEpisodeStats, ProgressState, SyncChange, SyncResponse},
};

#[derive(Clone)]
pub struct App {
    db: Arc<Db>,
    http: reqwest::Client,
}

impl App {
    pub fn new(db: Arc<Db>, http: reqwest::Client) -> Self {
        Self { db, http }
    }
}

type Result<T> = std::result::Result<T, AppError>;

#[derive(Clone)]
pub struct CursorPagination {
    pub limit: i64,
    pub cursor: Option<(chrono::DateTime<chrono::Utc>, String)>,
}

impl App {
    pub async fn get_podcast(&self, podcast_id: &str) -> Result<Option<Podcast>> {
        let podcast = self.db.get_podcast_by_id(podcast_id).await?;
        Ok(podcast)
    }

    pub async fn get_podcast_for_user(
        &self,
        username: &str,
        podcast_id: &str,
    ) -> Result<Option<Podcast>> {
        let podcast = self.db.get_podcast_for_user(username, podcast_id).await?;
        Ok(podcast)
    }

    pub async fn get_podcasts_for_user(
        &self,
        username: &str,
    ) -> Result<Vec<PodcastWithEpisodeStats>> {
        let podcasts = self.db.get_subscribed_podcasts_for_user(username).await?;
        Ok(podcasts)
    }

    pub async fn add_podcast(&self, feed_url: &str) -> Result<Podcast> {
        let existing = self.db.get_podcast_by_url(feed_url).await?;
        if let Some(podcast) = existing {
            return Ok(podcast);
        }

        let feed = crate::feed::get_feed(&self.http, feed_url).await?;

        let title = feed.title.as_ref().unwrap().content.clone();
        let id = feed.id.clone();

        let now = chrono::Utc::now();
        let logo_link = feed.logo.as_ref().map(|l| l.uri.clone());
        let icon_link = feed.icon.as_ref().map(|l| l.uri.clone());

        let image_link = logo_link
            .or(icon_link)
            .expect("image or logo link is required");

        let podcast = Podcast {
            id: id.clone(),
            title,
            description: feed
                .description
                .as_ref()
                .map(|d| d.content.clone())
                .unwrap_or_else(String::new),
            image_link,
            feed_url: feed_url.to_string(),
            feed_type: "rss".to_string(),
            created_at: now,
            last_updated: now,
        };
        self.db.insert_podcast(&podcast).await?;

        self.refresh_podcast(&id).await?;

        Ok(podcast)
    }

    pub async fn subscribe_to_podcast(&self, username: &str, podcast_id: &str) -> Result<()> {
        self.db.add_subscription(username, podcast_id).await?;
        Ok(())
    }

    pub async fn refresh_all_podcasts(&self) -> Result<()> {
        let podcasts = self.db.list_podcasts().await?;

        let mut set = JoinSet::new();
        for podcast in podcasts {
            let app = self.clone();
            set.spawn(async move {
                match app.refresh_podcast(&podcast.id).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!(
                            "error refreshing podcast {} ({}): {:?}",
                            &podcast.title, &podcast.id, e
                        );
                    }
                }
            });
        }

        while (set.join_next().await).is_some() {}

        Ok(())
    }

    pub async fn refresh_podcast(&self, podcast_id: &str) -> Result<()> {
        let podcast = self.db.get_podcast_by_id(podcast_id).await?;
        let Some(podcast) = podcast else {
            return Err(AppError::NotFound(
                "podcast".to_string(),
                podcast_id.to_string(),
            ));
        };

        let now = chrono::Utc::now();

        let feed = crate::feed::get_feed(&self.http, &podcast.feed_url).await?;
        let feed_episode_ids = feed
            .entries
            .iter()
            .filter(|item| crate::feed::has_audio(item))
            .map(|item| item.id.to_string())
            .collect::<Vec<_>>();

        let new_episode_ids = self
            .db
            .find_new_episode_ids(podcast_id, &feed_episode_ids)
            .await?;

        let new_episodes = feed
            .entries
            .iter()
            .filter(|item| new_episode_ids.contains(&item.id.to_string()))
            .collect::<Vec<_>>();

        for entry in new_episodes {
            let episode = match entry_to_episode(&podcast.id, entry, now) {
                Ok(episode) => episode,
                Err(e) => {
                    error!("error creating episode: {:?}", e);
                    continue;
                }
            };

            self.db.insert_episode(episode).await?;
        }

        Ok(())
    }

    pub async fn get_episodes_with_progress(
        &self,
        username: &str,
        podcast_id: &str,
        pagination: Option<CursorPagination>,
    ) -> Result<Vec<EpisodeWithProgress>> {
        let episodes = self
            .db
            .get_episodes_with_progress_for_podcast(
                username,
                podcast_id,
                pagination.map(|p| (p.limit, p.cursor)),
            )
            .await?;
        Ok(episodes)
    }

    pub async fn update_episode_progress(
        &self,
        username: &str,
        episode_id: &str,
        progress: i32,
        done: bool,
    ) -> Result<ProgressState> {
        let progress = self
            .db
            .update_progress(username, episode_id, progress, done)
            .await?
            .ok_or_else(|| AppError::NotFound("episode".to_string(), episode_id.to_string()))?;
        Ok(ProgressState {
            progress: progress.progress,
            done: progress.done,
        })
    }

    // ==========================================================================
    // Sync protocol
    // ==========================================================================

    /// Fetch episode changes for the user's subscriptions since `since_seq`,
    /// hydrate each change row with the full episode, and build the sync
    /// response including the opaque cursor and has_more flag.
    pub async fn get_sync_changes(
        &self,
        username: &str,
        since_seq: i64,
        limit: i64,
    ) -> Result<SyncResponse> {
        let mut rows = self.db.get_sync_changes(username, since_seq, limit).await?;

        // We asked for limit+1 rows; if we got that many there are more pages.
        let has_more = rows.len() as i64 > limit;
        if has_more {
            rows.truncate(limit as usize);
        }

        let mut changes = Vec::with_capacity(rows.len());
        for row in &rows {
            // TODO: handle row.op == "delete" by emitting an episode_tombstone
            // instead of hydrating the full episode.

            // Hydrate the episode. Since we skip deletions, the episode
            // should always exist.
            let episode = self
                .db
                .find_episode_by_id(&row.episode_id)
                .await?
                .ok_or_else(|| {
                    AppError::NotFound("episode".to_string(), row.episode_id.clone())
                })?;

            changes.push(SyncChange {
                seq: row.seq,
                change_type: "episode",
                op: "upsert",
                podcast_id: row.podcast_id.clone(),
                episode,
            });
        }

        // The next cursor is the seq of the last returned row, or the
        // original since_seq if there were no changes.
        let next_seq = rows.last().map(|r| r.seq).unwrap_or(since_seq);

        Ok(SyncResponse {
            server_time: chrono::Utc::now(),
            next_since: encode_sync_cursor(next_seq),
            has_more,
            changes,
        })
    }

    /// Return the latest change seq visible to this user, used for ETag.
    pub async fn get_latest_seq_for_user(&self, username: &str) -> Result<Option<i64>> {
        let seq = self.db.get_latest_seq_for_user(username).await?;
        Ok(seq)
    }
}

// ==============================================================================
// Sync cursor encoding — base64-wrapped seq number, opaque to clients.
// ==============================================================================

pub fn encode_sync_cursor(seq: i64) -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(seq.to_string().as_bytes())
}

pub fn decode_sync_cursor(cursor: &str) -> std::result::Result<i64, AppError> {
    use base64::prelude::*;
    let bytes = BASE64_STANDARD
        .decode(cursor.as_bytes())
        .map_err(|_| AppError::BadRequest("invalid sync cursor".into()))?;
    let s = String::from_utf8(bytes).map_err(|_| AppError::BadRequest("invalid sync cursor".into()))?;
    s.parse::<i64>()
        .map_err(|_| AppError::BadRequest("invalid sync cursor".into()))
}
