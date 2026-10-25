use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn healthz_answers_ok() {
    let request = Request::get("/healthz")
        .body(Body::empty())
        .expect("static request is well-formed");
    let response = pod_server::app()
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
    let response = pod_server::app()
        .oneshot(request)
        .await
        .expect("the router is infallible");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
