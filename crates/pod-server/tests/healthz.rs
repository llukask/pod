mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn healthz_answers_ok() {
    let request = Request::get("/healthz")
        .body(Body::empty())
        .expect("static request is well-formed");
    let response = pod_server::app(common::offline_state())
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
    let response = pod_server::app(common::offline_state())
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
