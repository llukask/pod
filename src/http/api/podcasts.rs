use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use base64::prelude::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::Serialize;

use crate::{
    app::CursorPagination,
    http::{
        auth::ApiUser,
        errors::{AppError, JsonAppError},
        AppState,
    },
    model::{EpisodeWithProgress, Podcast, PodcastWithEpisodeStats},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_podcasts).post(add_podcast))
        .route("/:id", get(get_podcast))
        .route("/:id/episodes", get(list_episodes))
}

async fn list_podcasts(
    user: ApiUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<PodcastWithEpisodeStats>>, JsonAppError> {
    let podcasts = state.app.get_podcasts_for_user(&user.username).await?;
    Ok(Json(podcasts))
}

async fn get_podcast(
    user: ApiUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Podcast>, JsonAppError> {
    let podcast = state
        .app
        .get_podcast_for_user(&user.username, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("podcast".to_string(), id))?;
    Ok(Json(podcast))
}

#[derive(Deserialize)]
struct AddPodcastRequest {
    feed_url: String,
}

async fn add_podcast(
    user: ApiUser,
    State(state): State<AppState>,
    Json(req): Json<AddPodcastRequest>,
) -> Result<Json<Podcast>, JsonAppError> {
    let podcast = state.app.add_podcast(&req.feed_url).await?;
    state
        .app
        .subscribe_to_podcast(&user.username, &podcast.id)
        .await?;
    Ok(Json(podcast))
}

async fn list_episodes(
    user: ApiUser,
    State(state): State<AppState>,
    Query(params): Query<EpisodeListParams>,
    Path(podcast_id): Path<String>,
) -> Result<Json<EpisodePage>, JsonAppError> {
    let pagination = params.to_pagination()?;
    let limit = pagination.limit;
    let episodes = state
        .app
        .get_episodes_with_progress(&user.username, &podcast_id, Some(pagination))
        .await?;

    // Only emit a next_page_token when the page is full, indicating
    // there may be more results.
    let next_page_token = if episodes.len() as i64 == limit {
        episodes
            .last()
            .map(|ep| encode_page_token(ep.episode.publication_date, &ep.episode.id))
    } else {
        None
    };

    Ok(Json(EpisodePage {
        items: episodes,
        next_page_token,
    }))
}

#[derive(Deserialize)]
struct EpisodeListParams {
    per_page: Option<u32>,
    page_token: Option<String>,
}

impl EpisodeListParams {
    fn to_pagination(&self) -> Result<CursorPagination, JsonAppError> {
        let per_page = self.per_page.unwrap_or(20).clamp(1, 100);
        let cursor = match &self.page_token {
            Some(token) => Some(decode_page_token(token)?),
            None => None,
        };

        Ok(CursorPagination {
            limit: per_page as i64,
            cursor,
        })
    }
}

/// Decode a compound page token of the form `{rfc3339}\n{episode_id}`, base64-encoded.
fn decode_page_token(token: &str) -> Result<(DateTime<Utc>, String), JsonAppError> {
    let decoded = BASE64_STANDARD
        .decode(token.as_bytes())
        .map_err(|_| AppError::OptionError)?;
    let s = String::from_utf8(decoded).map_err(|_| AppError::OptionError)?;
    let (date_str, id) = s.split_once('\n').ok_or(AppError::OptionError)?;
    let dt = DateTime::parse_from_rfc3339(date_str)
        .map_err(|_| AppError::OptionError)?
        .with_timezone(&Utc);
    Ok((dt, id.to_string()))
}

fn encode_page_token(dt: DateTime<Utc>, id: &str) -> String {
    let payload = format!("{}\n{}", dt.to_rfc3339(), id);
    BASE64_STANDARD.encode(payload.as_bytes())
}

#[derive(Serialize)]
struct EpisodePage {
    items: Vec<EpisodeWithProgress>,
    next_page_token: Option<String>,
}
