use thiserror::Error;

use crate::model::Episode;

#[derive(Debug, Clone)]
pub struct FeedRef {
    pub url: String,
    pub typ: String,
}

#[derive(Debug, Error)]
pub enum GetFeedError {
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("feed_rs error: {0}")]
    FeedRs(#[from] feed_rs::parser::ParseFeedError),
}

pub async fn get_feed(
    c: &reqwest::Client,
    url: &str,
) -> Result<feed_rs::model::Feed, GetFeedError> {
    let req = c.get(url).build();
    let res = c.execute(req?).await?;
    let text = res.text().await?;
    let parsed_feed = feed_rs::parser::parse(text.as_bytes())?;
    Ok(parsed_feed)
}

pub fn has_audio(e: &feed_rs::model::Entry) -> bool {
    let audio_content = e.media.iter().flat_map(|m| m.content.iter()).find(|e| {
        e.content_type
            .as_ref()
            .map(|mt| mt.essence_str() == "audio/mpeg")
            .unwrap_or(false)
    });

    audio_content.is_some()
}

pub fn entry_to_episode(
    podcast_id: &str,
    entry: &feed_rs::model::Entry,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Episode> {
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

        Ok(Episode {
            id: entry.id.clone(),
            podcast_id: podcast_id.to_string(),

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
        })
    } else {
        anyhow::bail!("no audio content found");
    }
}
