//! The web client's static files (`web_dir`): the bundle, the client-side
//! routes falling back to `index.html`, and the API keeping its own 404s.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Response, StatusCode, header};
use tower::ServiceExt;

/// The router over a scratch bundle: an `index.html` and one hashed asset.
fn app() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a scratch dir");
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>Pod</title>",
    )
    .expect("write index.html");
    std::fs::create_dir(dir.path().join("assets")).expect("create assets/");
    std::fs::write(dir.path().join("assets/index-abc123.js"), "console.log(1)")
        .expect("write the asset");
    (
        pod_server::app_with_web(common::offline_state(), dir.path()),
        dir,
    )
}

async fn get(path: &str) -> Response<Body> {
    let (app, _dir) = app();
    let request = axum::http::Request::get(path)
        .body(Body::empty())
        .expect("static request is well-formed");
    app.oneshot(request)
        .await
        .expect("the router is infallible")
}

async fn body(response: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body is in memory");
    String::from_utf8(bytes.to_vec()).expect("the body is UTF-8")
}

fn header(response: &Response<Body>, name: header::HeaderName) -> &str {
    response
        .headers()
        .get(&name)
        .unwrap_or_else(|| panic!("{name} is set"))
        .to_str()
        .expect("the header is ASCII")
}

#[tokio::test]
async fn root_serves_index_under_the_csp() {
    let response = get("/").await;
    assert_eq!(response.status(), StatusCode::OK);
    // Spec 2.2: show notes are inserted as HTML, so no inline script may run.
    let csp = header(&response, header::CONTENT_SECURITY_POLICY);
    assert!(csp.contains("script-src 'self'"), "{csp}");
    assert!(!csp.contains("unsafe-inline"), "{csp}");
    assert_eq!(header(&response, header::CACHE_CONTROL), "no-cache");
    assert!(body(response).await.contains("<title>Pod</title>"));
}

#[tokio::test]
async fn client_routes_fall_back_to_index() {
    let response = get("/podcasts/9c3e0000-0000-0000-0000-000000000000").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body(response).await.contains("<title>Pod</title>"));
}

#[tokio::test]
async fn hashed_assets_are_cached_for_good() {
    let response = get("/assets/index-abc123.js").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body(response).await, "console.log(1)");
}

#[tokio::test]
async fn a_missing_asset_is_not_index() {
    assert_eq!(get("/assets/gone.js").await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_api_paths_keep_the_json_404() {
    let response = get("/v1/nope").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(body(response).await.contains("\"not_found\""));
}

#[tokio::test]
async fn healthz_still_answers() {
    assert_eq!(get("/healthz").await.status(), StatusCode::OK);
}
