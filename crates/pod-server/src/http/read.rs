//! The read endpoints for the online web client (spec 4.5). The queries
//! live in [`crate::read`]; this module parses parameters and shapes
//! responses.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use color_eyre::eyre::WrapErr;
use pod_protocol::{
    EpisodePage, InboxPage, PodcastId, PodcastItem, PodcastList, SettingsResponse, clamp_page_limit,
};
use serde::Deserialize;

use super::AppState;
use super::error::ApiError;
use crate::auth::AuthSession;
use crate::read::{self, PageCursor, PageRequest};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/podcasts", get(podcasts))
        .route("/v1/podcasts/{id}", get(podcast))
        .route("/v1/podcasts/{id}/episodes", get(podcast_episodes))
        .route("/v1/inbox", get(inbox))
        .route("/v1/settings", get(settings))
}

// ==============================================================================
// Parameters
// ==============================================================================

/// `?cursor=&limit=`, read as strings so that an empty value means absent
/// (the spec writes the parameters that way) and so that a bad `cursor` gets
/// its own error code.
#[derive(Debug, Deserialize)]
struct PageParams {
    cursor: Option<String>,
    limit: Option<String>,
}

impl PageParams {
    fn parse(params: Result<Query<Self>, QueryRejection>) -> Result<PageRequest, ApiError> {
        let Query(params) =
            params.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
        let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
        let limit = non_empty(params.limit)
            .map(|limit| limit.parse::<u32>())
            .transpose()
            .map_err(|_| ApiError::bad_request("`limit` must be a non-negative integer."))?;
        let after = non_empty(params.cursor)
            .map(|cursor| PageCursor::decode(&cursor))
            .transpose()
            .map_err(|_| ApiError::invalid_cursor())?;
        Ok(PageRequest {
            after,
            limit: clamp_page_limit(limit),
        })
    }
}

fn parse_podcast_id(id: Result<Path<String>, PathRejection>) -> Result<PodcastId, ApiError> {
    let Path(id) = id.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
    id.parse()
        .map_err(|_| ApiError::bad_request("The podcast id is not a UUID."))
}

// ==============================================================================
// Handlers
// ==============================================================================

async fn podcasts(
    State(state): State<AppState>,
    session: AuthSession,
) -> Result<Json<PodcastList>, ApiError> {
    let items = read::podcasts(&state.pool, session.user_id, None)
        .await
        .context("list the subscribed podcasts")?;
    Ok(Json(PodcastList { items }))
}

async fn podcast(
    State(state): State<AppState>,
    session: AuthSession,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<PodcastItem>, ApiError> {
    let podcast_id = parse_podcast_id(id)?;
    let item = read::podcasts(&state.pool, session.user_id, Some(podcast_id))
        .await
        .context("read one podcast")?
        .pop()
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(item))
}

async fn podcast_episodes(
    State(state): State<AppState>,
    session: AuthSession,
    id: Result<Path<String>, PathRejection>,
    params: Result<Query<PageParams>, QueryRejection>,
) -> Result<Json<EpisodePage>, ApiError> {
    let podcast_id = parse_podcast_id(id)?;
    let page = PageParams::parse(params)?;
    let page = read::episodes(&state.pool, session.user_id, podcast_id, page)
        .await
        .context("read a page of episodes")?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(EpisodePage {
        items: page.items,
        next_cursor: page.next_cursor.map(|cursor| cursor.encode()),
    }))
}

async fn inbox(
    State(state): State<AppState>,
    session: AuthSession,
    params: Result<Query<PageParams>, QueryRejection>,
) -> Result<Json<InboxPage>, ApiError> {
    let page = PageParams::parse(params)?;
    let page = read::inbox(&state.pool, session.user_id, state.clock.now(), page)
        .await
        .context("read a page of the inbox")?;
    Ok(Json(InboxPage {
        items: page.items,
        next_cursor: page.next_cursor.map(|cursor| cursor.encode()),
    }))
}

async fn settings(
    State(state): State<AppState>,
    session: AuthSession,
) -> Result<Json<SettingsResponse>, ApiError> {
    let settings = read::settings(&state.pool, session.user_id)
        .await
        .context("read the settings")?;
    Ok(Json(SettingsResponse { settings }))
}
