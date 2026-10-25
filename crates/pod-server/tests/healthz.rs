use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use pod_server::AppState;
use pod_server::clock::Clock;
use pod_server::directory::Directory;
use pod_server::http::rate_limit::RateLimiter;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use url::Url;

/// The router with a pool that never connects, which proves these routes
/// don't touch Postgres.
fn app() -> Router {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgresql://unused@localhost/unused")
        .expect("the URL parses");
    pod_server::app(AppState {
        pool,
        allow_registration: true,
        trust_fly_client_ip: false,
        auth_rate_limit: Arc::new(RateLimiter::auth()),
        clock: Clock::system(),
        directory: Arc::new(
            Directory::new(
                &Url::parse("https://itunes.apple.com").expect("valid URL literal"),
                "https://pod.example.org",
            )
            .expect("the directory client builds"),
        ),
    })
}

#[tokio::test]
async fn healthz_answers_ok() {
    let request = Request::get("/healthz")
        .body(Body::empty())
        .expect("static request is well-formed");
    let response = app()
        .oneshot(request)
        .await
        .expect("the router is infallible");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn unknown_route_is_not_found() {
    let request = Request::get("/v1/nope")
        .body(Body::empty())
        .expect("static request is well-formed");
    let response = app()
        .oneshot(request)
        .await
        .expect("the router is infallible");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body is in memory");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("the body is JSON");
    assert_eq!(body["error"]["code"], "not_found");
}
