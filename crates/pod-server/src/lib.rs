//! Pod sync server: HTTP API, feed poller and Postgres store (spec sections 2
//! to 4).

pub mod admin;
pub mod auth;
pub mod clock;
pub mod config;
pub mod db;
pub mod directory;
pub mod feed;
pub mod fetch;
pub mod housekeeping;
pub mod http;
pub mod ingest;
pub mod poller;
pub mod read;
pub mod sync;
pub mod version;

use std::path::Path;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{any, get};

pub use http::AppState;

/// Builds the API router. Tests drive it in-process via
/// `tower::ServiceExt::oneshot`, so it must not need a bound socket.
pub fn app(state: AppState) -> Router {
    api().fallback(not_found).with_state(state)
}

/// The API plus the web client built into `web_dir` (`http::web`). Unknown
/// `/v1/` paths still get the API's JSON `not_found`; every other path the
/// API doesn't claim belongs to the client.
pub fn app_with_web(state: AppState, web_dir: &Path) -> Router {
    api()
        .route("/v1/{*rest}", any(not_found))
        .fallback_service(http::web::routes(web_dir))
        .with_state(state)
}

fn api() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(http::auth::routes())
        .merge(http::directory::routes())
        .merge(http::sync::routes())
        .merge(http::read::routes())
}

async fn not_found() -> http::error::ApiError {
    http::error::ApiError::not_found()
}

/// Liveness only. It deliberately skips Postgres so a database blip doesn't
/// make the orchestrator restart an otherwise healthy server.
// TODO: decide on a separate readiness check against Postgres once sqlx lands.
async fn healthz() -> StatusCode {
    StatusCode::OK
}
