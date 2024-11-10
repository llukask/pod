use std::sync::Arc;

use tokio::task::JoinSet;

use crate::{
    db::Db,
    feed::entry_to_episode,
    http::errors::ApiError,
    model::{EpisodeWithProgress, Podcast, PodcastWithEpisodeStats},
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

type Result<T> = std::result::Result<T, ApiError>;

impl App {
    pub async fn get_podcast(&self, podcast_id: &str) -> Result<Option<Podcast>> {
        let podcast = self.db.get_podcast_by_id(podcast_id).await?;
        Ok(podcast)
    }

    pub async fn get_podcasts_for_user(
        &self,
        user_email: &str,
    ) -> Result<Vec<PodcastWithEpisodeStats>> {
        let podcasts = self.db.get_subscribed_podcasts_for_user(user_email).await?;
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

    pub async fn subscribe_to_podcast(&self, user_email: &str, podcast_id: &str) -> Result<()> {
        self.db.add_subscription(user_email, podcast_id).await?;
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
                        println!("Error refreshing podcast: {:?}", e);
                    }
                }
            });
        }

        while let Some(_) = set.join_next().await {}

        Ok(())
    }

    pub async fn refresh_podcast(&self, podcast_id: &str) -> Result<()> {
        let podcast = self.db.get_podcast_by_id(podcast_id).await?;
        let Some(podcast) = podcast else {
            return Err(ApiError::NotFound(
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
                    println!("Error creating episode: {:?}", e);
                    continue;
                }
            };

            self.db.insert_episode(episode).await?;
        }

        Ok(())
    }

    pub async fn get_episodes_with_progress(
        &self,
        user_email: &str,
        podcast_id: &str,
    ) -> Result<Vec<EpisodeWithProgress>> {
        let episodes = self
            .db
            .get_episodes_with_progress_for_podcast(user_email, podcast_id)
            .await?;
        Ok(episodes)
    }
}
