//! Pod sync server: HTTP API, feed poller and Postgres store (spec sections 2
//! to 4).

pub mod config;
pub mod db;
pub mod feed;
pub mod fetch;
pub mod version;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

/// Builds the HTTP router. Tests drive it in-process via
/// `tower::ServiceExt::oneshot`, so it must not need a bound socket.
pub fn app() -> Router {
    Router::new().route("/healthz", get(healthz))
}

/// Liveness only. It deliberately skips Postgres so a database blip doesn't
/// make the orchestrator restart an otherwise healthy server.
// TODO: decide on a separate readiness check against Postgres once sqlx lands.
async fn healthz() -> StatusCode {
    StatusCode::OK
}
