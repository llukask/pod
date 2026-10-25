//! `/v1/auth/*` handlers and the bearer token extractor (spec 4.1).

use std::time::Instant;

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use color_eyre::eyre::WrapErr;
use pod_protocol::{AuthRequest, MeResponse, TokenResponse};

use super::error::{ApiError, ApiJson};
use super::rate_limit::RateKey;
use super::{AppState, ClientIp};
use crate::auth::{
    self, AuthSession, LoginAttempt, LoginError, NewAccount, NewSession, RegisterError,
    SessionToken, Username,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/register", post(register))
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/me", get(me))
}

async fn register(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(request): ApiJson<AuthRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    if !state.allow_registration {
        return Err(ApiError::registration_disabled());
    }
    let account = NewAccount::parse(request).map_err(|invalid| ApiError::bad_request(invalid.0))?;
    check_rate_limit(&state, ip, Some(&account.username))?;
    let session = auth::register(&state.pool, account, state.clock.now())
        .await
        .map_err(|error| match error {
            RegisterError::UsernameTaken => ApiError::username_taken(),
            RegisterError::Internal(report) => report.wrap_err("register the account").into(),
        })?;
    tracing::info!(user_id = %session.user_id, device_id = %session.device_id, "registered");
    Ok(Json(token_response(session)))
}

async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(request): ApiJson<AuthRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let attempt =
        LoginAttempt::parse(request).map_err(|invalid| ApiError::bad_request(invalid.0))?;
    check_rate_limit(&state, ip, attempt.username.as_ref())?;
    let session = auth::login(&state.pool, attempt, state.clock.now())
        .await
        .map_err(|error| match error {
            LoginError::InvalidCredentials => ApiError::invalid_credentials(),
            LoginError::Internal(report) => report.wrap_err("log in").into(),
        })?;
    tracing::info!(user_id = %session.user_id, device_id = %session.device_id, "logged in");
    Ok(Json(token_response(session)))
}

async fn logout(
    State(state): State<AppState>,
    session: AuthSession,
) -> Result<StatusCode, ApiError> {
    auth::logout(&state.pool, session.token_hash)
        .await
        .context("log out")?;
    tracing::info!(user_id = %session.user_id, device_id = %session.device_id, "logged out");
    Ok(StatusCode::NO_CONTENT)
}

async fn me(session: AuthSession) -> Json<MeResponse> {
    Json(MeResponse {
        user_id: session.user_id,
        username: session.username,
        device_id: session.device_id,
    })
}

/// Counts the attempt against the client IP and, when the request names a
/// valid one, the username.
fn check_rate_limit(
    state: &AppState,
    ip: std::net::IpAddr,
    username: Option<&Username>,
) -> Result<(), ApiError> {
    let mut keys = vec![RateKey::ip(ip)];
    keys.extend(username.cloned().map(RateKey::Username));
    state
        .auth_rate_limit
        .check(&keys, Instant::now())
        .map_err(ApiError::rate_limited)
}

fn token_response(session: NewSession) -> TokenResponse {
    TokenResponse {
        token: session.token.expose().to_owned(),
        device_id: session.device_id,
        user_id: session.user_id,
        expires_at: session.expires_at,
    }
}

/// Requires `Authorization: Bearer <token>` naming an unexpired session.
/// Every failure is the same `401 unauthenticated`, so a client can't tell
/// a garbled token from an expired one.
impl FromRequestParts<AppState> for AuthSession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
            // The auth scheme is case-insensitive (RFC 9110 section 11.1).
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .and_then(|(_, token)| SessionToken::parse(token.trim()))
            .ok_or_else(ApiError::unauthenticated)?;
        auth::authenticate(&state.pool, &token, state.clock.now())
            .await
            .context("authenticate the bearer token")?
            .ok_or_else(ApiError::unauthenticated)
    }
}
