//! Pod sync server: HTTP API, feed poller and Postgres store (spec sections 2
//! to 4).

pub mod auth;
pub mod clock;
pub mod config;
pub mod db;
pub mod feed;
pub mod fetch;
pub mod http;
pub mod ingest;
pub mod poller;
pub mod sync;
pub mod version;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

pub use http::AppState;

/// Builds the HTTP router. Tests drive it in-process via
/// `tower::ServiceExt::oneshot`, so it must not need a bound socket.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(http::auth::routes())
        .merge(http::sync::routes())
        .fallback(async || http::error::ApiError::not_found())
        .with_state(state)
}

/// Liveness only. It deliberately skips Postgres so a database blip doesn't
/// make the orchestrator restart an otherwise healthy server.
// TODO: decide on a separate readiness check against Postgres once sqlx lands.
async fn healthz() -> StatusCode {
    StatusCode::OK
}
