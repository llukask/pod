//! `GET /v1/directory/search` (spec 4.6).

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use color_eyre::eyre::WrapErr;
use pod_protocol::DirectorySearchResponse;
use serde::Deserialize;

use super::AppState;
use super::error::ApiError;
use crate::auth::AuthSession;
use crate::directory::{self, SearchTerm, clamp_limit};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/directory/search", get(search))
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
    limit: Option<u32>,
}

/// Authenticated only so that the endpoint isn't an open proxy to iTunes;
/// results don't depend on the user.
async fn search(
    State(state): State<AppState>,
    session: AuthSession,
    params: Result<Query<SearchParams>, QueryRejection>,
) -> Result<Json<DirectorySearchResponse>, ApiError> {
    let Query(params) = params.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
    let term = params
        .q
        .as_deref()
        .and_then(SearchTerm::parse)
        .ok_or_else(|| ApiError::bad_request("The search term `q` is missing or blank."))?;
    let entries = state
        .directory
        .search(&term, state.clock.now())
        .await
        .map_err(|error| {
            // The term is user input, so it stays out of warnings; the
            // directory logs it at debug level.
            tracing::warn!(
                user_id = %session.user_id,
                error = format!("{:#}", color_eyre::eyre::Report::new(error)),
                "directory search failed"
            );
            ApiError::upstream_unavailable()
        })?;
    let limit = clamp_limit(params.limit).min(entries.len());
    let results = directory::resolve_podcast_ids(&state.pool, &entries[..limit])
        .await
        .context("resolve directory podcast ids")?;
    Ok(Json(DirectorySearchResponse { results }))
}
