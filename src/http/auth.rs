use crate::http::errors::ApiError;
use anyhow::Result;
use axum::{
    extract::{FromRequestParts, Query, State},
    http::request::Parts,
    response::{IntoResponse, Redirect},
    routing::get,
    Extension, Router,
};
use axum_extra::extract::{cookie::Cookie, PrivateCookieJar};
use chrono::{Duration, Utc};
use oauth2::{
    basic::BasicClient, reqwest::async_http_client, AuthUrl, AuthorizationCode, ClientId,
    ClientSecret, RedirectUrl, TokenResponse as _, TokenUrl,
};

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/google_callback", get(google_callback))
}

pub fn build_google_oauth_client(
    client_id: String,
    client_secret: String,
    base_url: &str,
) -> BasicClient {
    let redirect_url = format!("{base_url}/auth/google_callback");

    let auth_url = AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())
        .expect("Invalid authorization endpoint URL");
    let token_url = TokenUrl::new("https://www.googleapis.com/oauth2/v3/token".to_string())
        .expect("Invalid token endpoint URL");

    BasicClient::new(
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
        auth_url,
        Some(token_url),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_url).expect("Invalid redirect url"))
}

#[derive(Debug, serde::Deserialize)]
pub struct AuthRequest {
    code: String,
}

#[derive(Debug, serde::Deserialize, sqlx::FromRow, Clone)]
pub struct UserProfile {
    pub email: String,
}

#[derive(Debug, Clone)]
pub enum MaybeUser {
    LoggedIn(UserProfile),
    LoggedOut,
}

pub async fn google_callback(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Query(query): Query<AuthRequest>,
    Extension(oauth_client): Extension<BasicClient>,
) -> Result<impl IntoResponse, ApiError> {
    let token = oauth_client
        .exchange_code(AuthorizationCode::new(query.code))
        .request_async(async_http_client)
        .await
        .expect("Failed to exchange code");

    let profile = state
        .http
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .bearer_auth(token.access_token().secret().to_owned())
        .send()
        .await
        .expect("Failed to fetch user profile");

    let profile = profile
        .json::<UserProfile>()
        .await
        .expect("Failed to parse user profile");

    let Some(secs) = token.expires_in() else {
        panic!("Token does not expire");
    };

    let secs: i64 = secs
        .as_secs()
        .try_into()
        .expect("Token expiration too large");

    let max_age = Utc::now() + Duration::seconds(secs);

    let cookie = Cookie::build(("sid", token.access_token().secret().to_owned()))
        .domain(state.cookie_domain)
        .path("/")
        .secure(true)
        .http_only(true)
        .max_age(time::Duration::seconds(secs));

    let user = state.db.insert_user(&profile.email).await?;
    state
        .db
        .update_user_session(user.id, token.access_token().secret(), max_age)
        .await?;

    Ok((jar.add(cookie), Redirect::to("/dash")))
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

        Ok(Self { email: user.email })
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

        Ok(Self::LoggedIn(UserProfile { email: user.email }))
    }
}
