// ==============================================================================
// API client
// ==============================================================================
//
// Thin wrappers over `gloo_net::http::Request` for the pod-server REST surface
// (`/api/v1/*`) and for the iTunes podcast search used by the "Add Podcast"
// panel. All authenticated calls go through `request_json`, which:
//
//   - Attaches `Authorization: Bearer <token>` from the global state.
//   - Decodes JSON responses (or `()` for 204).
//   - On 401, clears the token so the auth screen re-mounts.

use gloo_net::http::Request;
use leptos::prelude::GetUntracked;
use pod_model::{
    Episode, EpisodeWithProgress, InboxEpisode, Podcast, PodcastWithEpisodeStats, ProgressState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::use_app_state;

const API: &str = "/api/v1";
pub const APPLE_SEARCH_API: &str = "https://itunes.apple.com/search";

#[derive(Debug)]
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

impl From<gloo_net::Error> for ApiError {
    fn from(e: gloo_net::Error) -> Self {
        ApiError(e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError(e.to_string())
    }
}

/// Send a JSON request to the server and decode the response body.
///
/// `body` is serialized with `serde_json::to_string` if `Some`. The empty-body
/// case (used by GET / DELETE) skips the `Content-Type` header, matching
/// fetch defaults.
async fn request_json<T, B>(method: &str, path: &str, body: Option<&B>) -> Result<T, ApiError>
where
    T: serde::de::DeserializeOwned,
    B: serde::Serialize,
{
    let url = format!("{}{}", API, path);
    let mut req = match method {
        "GET" => Request::get(&url),
        "POST" => Request::post(&url),
        "DELETE" => Request::delete(&url),
        "PUT" => Request::put(&url),
        m => return Err(ApiError(format!("unsupported method: {m}"))),
    };

    if let Some(token) = use_app_state().token.get_untracked() {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }

    let res = if let Some(b) = body {
        req.header("Content-Type", "application/json")
            .body(serde_json::to_string(b)?)?
            .send()
            .await?
    } else {
        req.send().await?
    };

    let status = res.status();
    if status == 401 {
        // Server rejected the session — clear local credentials so the auth
        // screen takes over on the next render.
        use_app_state().force_logout();
        return Err(ApiError("Session expired".to_string()));
    }
    if !(200..300).contains(&status) {
        // Try to parse `{ "error": "..." }` from the body, otherwise fall
        // back to a generic message.
        let txt = res.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&txt)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
            .unwrap_or_else(|| format!("Request failed ({status})"));
        return Err(ApiError(msg));
    }

    // 204 / empty body → use serde_json's null parser.
    let txt = res.text().await.unwrap_or_default();
    if txt.is_empty() {
        return serde_json::from_str("null").map_err(Into::into);
    }
    serde_json::from_str(&txt).map_err(Into::into)
}

// ------------------------------------------------------------------------------
// Auth
// ------------------------------------------------------------------------------

#[derive(Serialize)]
pub struct AuthRequest<'a> {
    pub username: &'a str,
    pub password: &'a str,
}

#[derive(Deserialize)]
pub struct AuthResponse {
    pub token: String,
    #[allow(dead_code)]
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

pub async fn login(username: &str, password: &str) -> Result<AuthResponse, ApiError> {
    request_json("POST", "/auth/login", Some(&AuthRequest { username, password })).await
}

pub async fn register(username: &str, password: &str) -> Result<AuthResponse, ApiError> {
    request_json(
        "POST",
        "/auth/register",
        Some(&AuthRequest { username, password }),
    )
    .await
}

pub async fn logout() -> Result<(), ApiError> {
    let _: Value = request_json::<Value, ()>("POST", "/auth/logout", None).await?;
    Ok(())
}

// ------------------------------------------------------------------------------
// Podcasts
// ------------------------------------------------------------------------------

#[derive(Serialize)]
struct FeedUrlBody<'a> {
    feed_url: &'a str,
}

pub async fn list_podcasts() -> Result<Vec<PodcastWithEpisodeStats>, ApiError> {
    request_json::<_, ()>("GET", "/podcasts", None).await
}

pub async fn subscribe_podcast(feed_url: &str) -> Result<Podcast, ApiError> {
    request_json("POST", "/podcasts", Some(&FeedUrlBody { feed_url })).await
}

pub async fn get_podcast(id: &str) -> Result<Podcast, ApiError> {
    let path = format!("/podcasts/{}", urlencoding::encode(id));
    request_json::<_, ()>("GET", &path, None).await
}

#[derive(Deserialize)]
pub struct EpisodesPage {
    pub items: Vec<EpisodeWithProgress>,
    pub next_page_token: Option<String>,
}

pub async fn list_episodes(
    podcast_id: &str,
    page_token: Option<&str>,
    per_page: u32,
) -> Result<EpisodesPage, ApiError> {
    let mut path = format!(
        "/podcasts/{}/episodes?per_page={}",
        urlencoding::encode(podcast_id),
        per_page
    );
    if let Some(t) = page_token {
        path.push_str("&page_token=");
        path.push_str(&urlencoding::encode(t));
    }
    request_json::<_, ()>("GET", &path, None).await
}

// ------------------------------------------------------------------------------
// Inbox
// ------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct InboxPage {
    pub items: Vec<InboxEpisode>,
    pub next_page_token: Option<String>,
}

pub async fn list_inbox(page_token: Option<&str>, per_page: u32) -> Result<InboxPage, ApiError> {
    let mut path = format!("/episodes/inbox?per_page={}", per_page);
    if let Some(t) = page_token {
        path.push_str("&page_token=");
        path.push_str(&urlencoding::encode(t));
    }
    request_json::<_, ()>("GET", &path, None).await
}

// ------------------------------------------------------------------------------
// Episode progress
// ------------------------------------------------------------------------------

#[derive(Serialize)]
struct ProgressBody {
    progress: i32,
    done: bool,
}

pub async fn report_progress(
    episode_id: &str,
    progress: i32,
    done: bool,
) -> Result<ProgressState, ApiError> {
    let path = format!(
        "/episodes/{}/progress",
        urlencoding::encode(episode_id)
    );
    request_json("POST", &path, Some(&ProgressBody { progress, done })).await
}

// ------------------------------------------------------------------------------
// iTunes podcast search (used by the "Add Podcast" search panel)
// ------------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
pub struct AppleSearchResult {
    #[serde(rename = "trackName")]
    pub track_name: Option<String>,
    #[serde(rename = "collectionName")]
    pub collection_name: Option<String>,
    #[serde(rename = "artistName")]
    pub artist_name: Option<String>,
    #[serde(rename = "feedUrl")]
    pub feed_url: Option<String>,
    #[serde(rename = "artworkUrl600")]
    pub artwork_url_600: Option<String>,
    #[serde(rename = "artworkUrl100")]
    pub artwork_url_100: Option<String>,
    #[serde(rename = "primaryGenreName")]
    pub primary_genre_name: Option<String>,
    #[serde(rename = "releaseDate")]
    pub release_date: Option<String>,
}

#[derive(Deserialize)]
pub struct AppleSearchEnvelope {
    pub results: Vec<AppleSearchResult>,
}

/// Hit `https://itunes.apple.com/search` with `media=podcast&entity=podcast`.
/// Apple's CDN returns CORS headers so this is callable straight from the
/// browser without a proxy.
pub async fn search_apple_podcasts(query: &str, limit: u32) -> Result<AppleSearchEnvelope, ApiError> {
    let url = format!(
        "{}?media=podcast&entity=podcast&limit={}&term={}",
        APPLE_SEARCH_API,
        limit,
        urlencoding::encode(query),
    );
    let res = Request::get(&url).send().await?;
    if !(200..300).contains(&res.status()) {
        return Err(ApiError("Apple search failed".to_string()));
    }
    let txt = res.text().await?;
    serde_json::from_str(&txt).map_err(Into::into)
}

/// Used by `playEpisode` so the audio element knows where to fetch from
/// when only an `Episode` (no progress wrapper) is at hand.
#[allow(dead_code)]
pub fn _episode_audio_url(ep: &Episode) -> &str {
    ep.audio_url.as_str()
}
