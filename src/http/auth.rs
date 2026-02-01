use crate::http::errors::ApiError;
use anyhow::Result;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use askama::Template;
use axum::{
    extract::{FromRequestParts, State},
    http::request::Parts,
    response::{IntoResponse, Redirect},
    routing::get,
    Form, Router,
};
use axum_extra::extract::{cookie::Cookie, PrivateCookieJar};
use base64::{prelude::BASE64_STANDARD, Engine as _};
use chrono::{Duration, Utc};
use rand::RngCore;

use super::AppState;

const SESSION_DURATION_DAYS: i64 = 30;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(login_page).post(login))
        .route("/register", get(register_page).post(register))
        .route("/logout", get(logout))
}

#[derive(Debug, serde::Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, serde::Deserialize)]
struct RegisterRequest {
    username: String,
    password: String,
}

#[derive(Debug, serde::Deserialize, sqlx::FromRow, Clone)]
pub struct UserProfile {
    pub username: String,
}

#[derive(Debug, Clone)]
pub enum MaybeUser {
    LoggedIn(UserProfile),
    LoggedOut,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "register.html")]
struct RegisterTemplate {
    error: Option<String>,
}

async fn login_page() -> impl IntoResponse {
    LoginTemplate { error: None }.into_response()
}

async fn login(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Form(req): Form<LoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(user) = state.db.find_user_by_username(&req.username).await? else {
        return Ok((
            jar,
            LoginTemplate {
                error: Some("Invalid username or password".to_string()),
            },
        )
            .into_response());
    };

    let parsed_hash = PasswordHash::new(&user.password_hash).map_err(|_| ApiError::OptionError)?;
    if Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .is_err()
    {
        return Ok((
            jar,
            LoginTemplate {
                error: Some("Invalid username or password".to_string()),
            },
        )
            .into_response());
    }

    let (jar, _) = create_session(&state, jar, user.id).await?;
    Ok((jar, Redirect::to("/dash")).into_response())
}

async fn register_page(State(state): State<AppState>) -> impl IntoResponse {
    if !state.allow_registration {
        return Redirect::to("/auth/login").into_response();
    }
    RegisterTemplate { error: None }.into_response()
}

async fn register(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Form(req): Form<RegisterRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if !state.allow_registration {
        return Ok(Redirect::to("/auth/login").into_response());
    }

    if req.username.is_empty() || req.password.is_empty() {
        return Ok((
            jar,
            RegisterTemplate {
                error: Some("Username and password are required".to_string()),
            },
        )
            .into_response());
    }

    if state.db.find_user_by_username(&req.username).await?.is_some() {
        return Ok((
            jar,
            RegisterTemplate {
                error: Some("Username is already taken".to_string()),
            },
        )
            .into_response());
    }

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|_| ApiError::OptionError)?
        .to_string();

    let user = state.db.insert_user(&req.username, &password_hash).await?;

    let (jar, _) = create_session(&state, jar, user.id).await?;
    Ok((jar, Redirect::to("/dash")).into_response())
}

async fn logout(jar: PrivateCookieJar) -> impl IntoResponse {
    let jar = jar.remove(Cookie::from("sid"));
    (jar, Redirect::to("/auth/login"))
}

async fn create_session(
    state: &AppState,
    jar: PrivateCookieJar,
    user_id: uuid::Uuid,
) -> Result<(PrivateCookieJar, String), ApiError> {
    let mut token_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut token_bytes);
    let session_token = BASE64_STANDARD.encode(token_bytes);

    let expires_at = Utc::now() + Duration::days(SESSION_DURATION_DAYS);
    let max_age_secs = SESSION_DURATION_DAYS * 24 * 60 * 60;

    state
        .db
        .update_user_session(user_id, &session_token, expires_at)
        .await?;

    let cookie = Cookie::build(("sid", session_token.clone()))
        .domain(state.cookie_domain.clone())
        .path("/")
        .secure(true)
        .http_only(true)
        .max_age(time::Duration::seconds(max_age_secs));

    Ok((jar.add(cookie), session_token))
}

#[axum::async_trait]
impl FromRequestParts<AppState> for UserProfile {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let cookiejar: PrivateCookieJar =
            PrivateCookieJar::from_request_parts(parts, state).await?;

        let Some(cookie) = cookiejar.get("sid").map(|cookie| cookie.value().to_owned()) else {
            return Err(ApiError::Unauthorized);
        };

        let Some(user) = state.db.find_user_by_session_id(&cookie).await? else {
            return Err(ApiError::Unauthorized);
        };

        Ok(Self {
            username: user.username,
        })
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let cookiejar: PrivateCookieJar =
            PrivateCookieJar::from_request_parts(parts, state).await?;

        let Some(cookie) = cookiejar.get("sid").map(|cookie| cookie.value().to_owned()) else {
            return Ok(Self::LoggedOut);
        };

        let Some(user) = state.db.find_user_by_session_id(&cookie).await? else {
            return Ok(Self::LoggedOut);
        };

        Ok(Self::LoggedIn(UserProfile {
            username: user.username,
        }))
    }
}
