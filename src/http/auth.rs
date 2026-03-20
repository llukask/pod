use anyhow::Result;
use base64::{prelude::BASE64_STANDARD, Engine as _};
use chrono::{Duration, Utc};
use rand::RngCore;

use super::errors::{AppError, JsonAppError};
use super::AppState;

const SESSION_DURATION_DAYS: i64 = 30;

/// Creates a session token and stores it in the DB. Returns (token, expires_at).
pub async fn create_session_token(
    state: &AppState,
    user_id: uuid::Uuid,
) -> Result<(String, chrono::DateTime<chrono::Utc>), AppError> {
    let mut token_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut token_bytes);
    let session_token = BASE64_STANDARD.encode(token_bytes);

    let expires_at = Utc::now() + Duration::days(SESSION_DURATION_DAYS);

    state
        .db
        .update_user_session(user_id, &session_token, expires_at)
        .await?;

    Ok((session_token, expires_at))
}

/// API user extractor — reads Bearer token from Authorization header.
pub struct ApiUser {
    pub username: String,
    pub session_token: String,
}

#[axum::async_trait]
impl axum::extract::FromRequestParts<AppState> for ApiUser {
    type Rejection = JsonAppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let auth_header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(JsonAppError(AppError::Unauthorized))?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or(JsonAppError(AppError::Unauthorized))?;

        let user = state
            .db
            .find_user_by_session_id(token)
            .await
            .map_err(AppError::from)?
            .ok_or(JsonAppError(AppError::Unauthorized))?;

        Ok(Self {
            username: user.username,
            session_token: token.to_owned(),
        })
    }
}
