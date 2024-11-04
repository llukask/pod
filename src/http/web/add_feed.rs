use askama_axum::IntoResponse;
use axum::{extract::State, response::Redirect, Form};
use feed_rs::model::Feed;
use serde::Deserialize;

use crate::db::{Db, Episode, Podcast};

use crate::http::{auth::UserProfile, errors::ApiError, AppState};

#[derive(Deserialize)]
pub struct AddFeedRequest {
    pub feed_url: String,
}

pub async fn add_feed(
    user: UserProfile,
    State(state): State<AppState>,
    Form(req): Form<AddFeedRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let existing = state.db.get_podcast_by_url(&req.feed_url).await?;
    if let Some(_) = existing {
        return Ok(Redirect::to("/dash"));
    } else {
        let feed = crate::feed::get_feed(&state.http, &req.feed_url).await?;
        let podcast = add_new_feed_to_db(&req.feed_url, &feed, state.db.as_ref()).await?;
        state.db.add_subscription(&user.email, &podcast.id).await?;

        Ok(Redirect::to("/dash"))
    }
}

async fn add_new_feed_to_db(url: &str, feed: &Feed, db: &Db) -> Result<Podcast, ApiError> {
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
        feed_url: url.to_string(),
        feed_type: "rss".to_string(),
        created_at: now,
        last_updated: now,
    };
    db.insert_podcast(&podcast).await?;

    for entry in &feed.entries {
        let media = entry.media.iter().find(|m| {
            m.content.iter().any(|c| {
                c.content_type
                    .as_ref()
                    .map(|mt| mt.essence_str() == "audio/mpeg")
                    .unwrap_or(false)
            })
        });

        if let Some(media) = media {
            let audio_content = media
                .content
                .iter()
                .find(|c| {
                    c.content_type
                        .as_ref()
                        .map(|mt| mt.essence_str() == "audio/mpeg")
                        .unwrap_or(false)
                })
                .expect("audio content is required");

            let duration = media
                .duration
                .or(audio_content.duration)
                .map(|d| d.as_secs().try_into().unwrap())
                .unwrap_or(0);

            let thumbnail_url = media.thumbnails.first().map(|t| t.image.uri.clone());

            let summary_text = entry.summary.as_ref();

            let episode = Episode {
                id: entry.id.clone(),
                podcast_id: id.clone(),

                title: entry.title.as_ref().unwrap().content.clone(),
                summary: entry
                    .summary
                    .as_ref()
                    .map(|s| s.content.clone())
                    .unwrap_or_else(String::new),
                summary_type: summary_text
                    .map(|s| s.content_type.essence_str().to_string())
                    .unwrap_or_else(String::new),

                publication_date: entry.published.unwrap_or(now),

                audio_url: audio_content
                    .url
                    .as_ref()
                    .expect("audio url is required")
                    .as_str()
                    .to_string(),
                audio_type: audio_content
                    .content_type
                    .as_ref()
                    .expect("audio type is required")
                    .essence_str()
                    .to_string(),
                audio_duration: duration,

                thumbnail_url,

                created_at: now,
                last_updated: now,
            };

            db.insert_episode(episode).await?;
        } else {
            eprintln!("no media content found for episode: {}", entry.id);
            eprintln!("{:#?}", entry);
        }
    }

    Ok(podcast)
}
