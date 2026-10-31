//! `/v1/sonos/*` (spec 13.2). The operations live in [`crate::sonos`];
//! this module maps them to responses.
//!
//! Every authenticated user may see and control every player: a household
//! shares its speakers, and the server is meant for one household.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use pod_protocol::{
    SonosCommand, SonosPlayRequest, SonosPlayer, SonosPlayersResponse, SonosReleased,
};

use super::AppState;
use super::error::{ApiError, ApiJson};
use crate::auth::AuthSession;
use crate::sonos::{Sonos, SonosError};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/sonos/players", get(players))
        .route("/v1/sonos/players/{id}/play", post(play))
        .route("/v1/sonos/players/{id}/command", post(command))
        .route("/v1/sonos/players/{id}/release", post(release))
}

/// The service, or `404` for every player while Sonos is off.
fn enabled(state: &AppState) -> Result<&Arc<Sonos>, ApiError> {
    state.sonos.as_ref().ok_or_else(ApiError::not_found)
}

fn api_error(user: &AuthSession, error: SonosError) -> ApiError {
    match error {
        SonosError::NotFound => ApiError::not_found(),
        SonosError::NoSession => ApiError::no_session(),
        SonosError::Speaker(error) => {
            tracing::warn!(
                user_id = %user.user_id,
                error = format!("{:#}", color_eyre::eyre::Report::new(error)),
                "a Sonos speaker failed"
            );
            ApiError::speaker_unavailable()
        }
        SonosError::Internal(report) => report.into(),
    }
}

/// An empty list while Sonos is off, which tells clients to hide it.
async fn players(
    State(state): State<AppState>,
    session: AuthSession,
) -> Result<Json<SonosPlayersResponse>, ApiError> {
    let players = match &state.sonos {
        Some(sonos) => sonos
            .players(&state.pool, session.user_id)
            .await
            .map_err(|error| api_error(&session, error))?,
        None => Vec::new(),
    };
    Ok(Json(SonosPlayersResponse { players }))
}

async fn play(
    State(state): State<AppState>,
    session: AuthSession,
    Path(player_id): Path<String>,
    ApiJson(request): ApiJson<SonosPlayRequest>,
) -> Result<Json<SonosPlayer>, ApiError> {
    let player = enabled(&state)?
        .play(&state.pool, session.user_id, &player_id, request)
        .await
        .map_err(|error| api_error(&session, error))?;
    Ok(Json(player))
}

async fn command(
    State(state): State<AppState>,
    session: AuthSession,
    Path(player_id): Path<String>,
    ApiJson(command): ApiJson<SonosCommand>,
) -> Result<Json<SonosPlayer>, ApiError> {
    let player = enabled(&state)?
        .command(
            &state.pool,
            session.user_id,
            &player_id,
            command,
            state.clock.now(),
        )
        .await
        .map_err(|error| api_error(&session, error))?;
    Ok(Json(player))
}

async fn release(
    State(state): State<AppState>,
    session: AuthSession,
    Path(player_id): Path<String>,
) -> Result<Json<SonosReleased>, ApiError> {
    let released = enabled(&state)?
        .release(&state.pool, session.user_id, &player_id, state.clock.now())
        .await
        .map_err(|error| api_error(&session, error))?;
    Ok(Json(released))
}
