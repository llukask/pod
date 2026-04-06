use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use base64::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    app::CursorPagination,
    http::{
        auth::ApiUser,
        errors::{AppError, JsonAppError},
        AppState,
    },
    model::{InboxEpisode, ProgressState},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/inbox", get(inbox))
        .route("/:id/progress", post(report_progress))
}

#[derive(Deserialize)]
struct ProgressReport {
    progress: i32,
    done: bool,
}

async fn report_progress(
    user: ApiUser,
    State(state): State<AppState>,
    Path(episode_id): Path<String>,
    Json(report): Json<ProgressReport>,
) -> Result<Json<ProgressState>, JsonAppError> {
    let progress = state
        .app
        .update_episode_progress(&user.username, &episode_id, report.progress, report.done)
        .await?;
    Ok(Json(progress))
}

// ==============================================================================
// Inbox — cross-podcast episode feed, excluding completed episodes
// ==============================================================================

#[derive(Deserialize)]
struct InboxParams {
    per_page: Option<u32>,
    page_token: Option<String>,
}

#[derive(Serialize)]
struct InboxPage {
    items: Vec<InboxEpisode>,
    next_page_token: Option<String>,
}

async fn inbox(
    user: ApiUser,
    State(state): State<AppState>,
    Query(params): Query<InboxParams>,
) -> Result<Json<InboxPage>, JsonAppError> {
    let per_page = params.per_page.unwrap_or(30).clamp(1, 100) as i64;
    let cursor = match &params.page_token {
        Some(token) => Some(decode_page_token(token)?),
        None => None,
    };

    let episodes = state
        .app
        .get_inbox_episodes(&user.username, CursorPagination { limit: per_page, cursor })
        .await?;

    let next_page_token = if episodes.len() as i64 == per_page {
        episodes
            .last()
            .map(|ep| encode_page_token(ep.episode.publication_date, &ep.episode.id))
    } else {
        None
    };

    Ok(Json(InboxPage {
        items: episodes,
        next_page_token,
    }))
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
