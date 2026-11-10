//! The read endpoints (spec 4.5): for the online web client, and for
//! chapters every client. The queries live in [`crate::read`] and
//! [`crate::chapters`]; this module parses parameters and shapes responses.

use std::time::Duration;

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use color_eyre::eyre::WrapErr;
use pod_protocol::{
    ChapterList, EpisodeId, EpisodePage, InboxPage, PodcastId, PodcastItem, PodcastList,
    SettingsResponse, clamp_page_limit,
};
use serde::Deserialize;

use super::AppState;
use super::error::ApiError;
use crate::auth::AuthSession;
use crate::chapters::{self, Lookup};
use crate::read::{self, EpisodeFilter, ListOrder, PageCursor, PageRequest};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/podcasts", get(podcasts))
        .route("/v1/podcasts/{id}", get(podcast))
        .route("/v1/podcasts/{id}/episodes", get(podcast_episodes))
        .route("/v1/episodes/{id}/chapters", get(episode_chapters))
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

/// `?season=&order=` of the episode list, read as strings for the same reasons
/// as [`PageParams`].
#[derive(Debug, Deserialize)]
struct FilterParams {
    season: Option<String>,
    order: Option<String>,
}

impl FilterParams {
    fn parse(params: Result<Query<Self>, QueryRejection>) -> Result<EpisodeFilter, ApiError> {
        let Query(params) =
            params.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
        let season = params
            .season
            .filter(|season| !season.is_empty())
            .map(|season| {
                season
                    .parse::<u32>()
                    .ok()
                    .and_then(|s| i32::try_from(s).ok())
            })
            .map(|season| {
                season.ok_or_else(|| {
                    ApiError::bad_request("`season` must be a non-negative integer.")
                })
            })
            .transpose()?;
        // A season starts from its first episode unless the client asks
        // otherwise (spec 4.5).
        let order = match params.order.as_deref().filter(|order| !order.is_empty()) {
            None if season.is_some() => ListOrder::OldestFirst,
            None => ListOrder::NewestFirst,
            Some("newest") => ListOrder::NewestFirst,
            Some("oldest") => ListOrder::OldestFirst,
            Some(_) => {
                return Err(ApiError::bad_request(
                    "`order` must be `newest` or `oldest`.",
                ));
            }
        };
        Ok(EpisodeFilter { season, order })
    }
}

async fn podcast_episodes(
    State(state): State<AppState>,
    session: AuthSession,
    id: Result<Path<String>, PathRejection>,
    params: Result<Query<PageParams>, QueryRejection>,
    filter: Result<Query<FilterParams>, QueryRejection>,
) -> Result<Json<EpisodePage>, ApiError> {
    let podcast_id = parse_podcast_id(id)?;
    let page = PageParams::parse(params)?;
    let filter = FilterParams::parse(filter)?;
    let page = read::episodes(&state.pool, session.user_id, podcast_id, filter, page)
        .await
        .context("read a page of episodes")?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(EpisodePage {
        items: page.items,
        next_cursor: page.next_cursor.map(|cursor| cursor.encode()),
    }))
}

/// How long a chapter lookup may take, every source included (spec 17.2).
const CHAPTERS_TIMEOUT: Duration = Duration::from_secs(25);

async fn episode_chapters(
    State(state): State<AppState>,
    session: AuthSession,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<ChapterList>, ApiError> {
    let Path(id) = id.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
    let episode_id: EpisodeId = id
        .parse()
        .map_err(|_| ApiError::bad_request("The episode id is not a UUID."))?;
    let lookup = chapters::lookup(
        &state.pool,
        &state.fetcher,
        session.user_id,
        episode_id,
        state.clock.now(),
    );
    let Ok(lookup) = tokio::time::timeout(CHAPTERS_TIMEOUT, lookup).await else {
        tracing::info!(%episode_id, "chapter lookup timed out");
        return Err(ApiError::chapters_unavailable());
    };
    match lookup.context("look up the episode's chapters")? {
        Lookup::NotFound => Err(ApiError::not_found()),
        Lookup::Chapters(chapters) => Ok(Json(ChapterList { chapters })),
        Lookup::Unavailable => Err(ApiError::chapters_unavailable()),
    }
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
