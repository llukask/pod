use reqwest::header;
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

/// Result of a conditional feed fetch. `NotModified` means the server
/// returned 304 and there is no new content to process.
pub enum FeedResult {
    Fetched {
        feed: feed_rs::model::Feed,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    NotModified,
}

/// Cached conditional-request headers from a previous fetch.
pub struct FeedCacheHeaders {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Fetch an RSS/Atom feed with HTTP conditional-request support.
///
/// Sends `If-None-Match` when a cached ETag is available, falling back to
/// `If-Modified-Since` when only a `Last-Modified` value was stored.
/// Returns `FeedResult::NotModified` when the server responds with 304.
pub async fn get_feed(
    c: &reqwest::Client,
    url: &str,
    cache: &FeedCacheHeaders,
) -> Result<FeedResult, GetFeedError> {
    let mut req = c.get(url);

    // Prefer ETag; fall back to Last-Modified for servers that don't
    // support ETags.
    if let Some(etag) = &cache.etag {
        req = req.header(header::IF_NONE_MATCH, etag);
    } else if let Some(last_modified) = &cache.last_modified {
        req = req.header(header::IF_MODIFIED_SINCE, last_modified);
    }

    let res = c.execute(req.build()?).await?;

    if res.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FeedResult::NotModified);
    }

    let response_etag = res
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let response_last_modified = res
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let text = res.text().await?;
    let feed = feed_rs::parser::parse(text.as_bytes())?;

    Ok(FeedResult::Fetched {
        feed,
        etag: response_etag,
        last_modified: response_last_modified,
    })
}

fn is_audio_mime(mt: &mime::Mime) -> bool {
    matches!(
        mt.essence_str(),
        "audio/mpeg"
            | "audio/mp3"
            | "audio/x-m4a"
            | "audio/mp4"
            | "audio/aac"
            | "video/mp4"
            | "video/x-m4v"
    )
}

pub fn has_audio(e: &feed_rs::model::Entry) -> bool {
    let audio_content = e.media.iter().flat_map(|m| m.content.iter()).find(|e| {
        e.content_type
            .as_ref()
            .map(|mt| is_audio_mime(mt))
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
                .map(|mt| is_audio_mime(mt))
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
                    .map(|mt| is_audio_mime(mt))
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
                .unwrap_or_default(),
            summary_type: summary_text
                .map(|s| s.content_type.essence_str().to_string())
                .unwrap_or_default(),

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
