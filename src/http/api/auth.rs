use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::http::{
    auth::{create_session_token, ApiUser},
    errors::{AppError, JsonAppError},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/login", post(login))
        .route("/register", post(register))
        .route("/logout", post(logout))
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct RegisterRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct AuthResponse {
    token: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
struct MeResponse {
    username: String,
}

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, JsonAppError> {
    let user = state
        .db
        .find_user_by_username(&req.username)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let parsed_hash = PasswordHash::new(&user.password_hash).map_err(|_| AppError::OptionError)?;
    Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .map_err(|_| AppError::Unauthorized)?;

    let (token, expires_at) = create_session_token(&state, user.id).await?;

    Ok(Json(AuthResponse { token, expires_at }))
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, JsonAppError> {
    if !state.allow_registration {
        return Err(AppError::Unauthorized.into());
    }

    if req.username.is_empty() || req.password.is_empty() {
        return Err(AppError::OptionError.into());
    }

    if state
        .db
        .find_user_by_username(&req.username)
        .await?
        .is_some()
    {
        return Err(AppError::OptionError.into());
    }

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|_| AppError::OptionError)?
        .to_string();

    let user = state.db.insert_user(&req.username, &password_hash).await?;

    let (token, expires_at) = create_session_token(&state, user.id).await?;

    Ok(Json(AuthResponse { token, expires_at }))
}

async fn logout(user: ApiUser, State(state): State<AppState>) -> Result<StatusCode, JsonAppError> {
    state.db.delete_session(&user.session_token).await?;
    Ok(StatusCode::OK)
}

async fn me(user: ApiUser) -> Result<Json<MeResponse>, JsonAppError> {
    Ok(Json(MeResponse {
        username: user.username,
    }))
}
