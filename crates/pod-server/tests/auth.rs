//! `/v1/auth/*` end to end through the router (spec 2.3 and 4.1).

mod auth_app;

use auth_app::{
    DAY_MS, HOUR_MS, PASSWORD, TRUSTED_PEER, TestApp, TestResponse, assert_error, plus_ms, token_of,
};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use pod_protocol::{DeviceId, UserId};
use serde_json::{Value, json};
use sqlx::PgPool;

// ==============================================================================
// Round trip
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn register_login_me_logout_round_trip(pool: PgPool) {
    let app = TestApp::new(pool);

    let registered = app
        .post_auth("/v1/auth/register", " Alice ", PASSWORD)
        .await;
    assert_eq!(registered.status, StatusCode::OK, "{}", registered.body);
    let user_id: UserId =
        serde_json::from_value(registered.body["user_id"].clone()).expect("a user id");
    let first_device: DeviceId =
        serde_json::from_value(registered.body["device_id"].clone()).expect("a device id");
    assert_eq!(user_id.as_uuid().get_version_num(), 7);
    assert_eq!(first_device.as_uuid().get_version_num(), 7);
    assert_eq!(
        registered.body["expires_at"],
        json!(plus_ms(app.now(), 90 * DAY_MS))
    );
    let first_token = token_of(&registered);

    let me = app.me(&first_token).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(
        me.body,
        json!({ "user_id": user_id, "username": "alice", "device_id": first_device })
    );

    // The username matches case-insensitively, and every login is a new
    // device.
    let logged_in = app.post_auth("/v1/auth/login", "ALICE", PASSWORD).await;
    assert_eq!(logged_in.status, StatusCode::OK, "{}", logged_in.body);
    assert_eq!(logged_in.body["user_id"], json!(user_id));
    assert_ne!(logged_in.body["device_id"], json!(first_device));
    let second_token = token_of(&logged_in);
    assert_ne!(second_token, first_token);
    let me = app.me(&second_token).await;
    assert_eq!(me.body["device_id"], logged_in.body["device_id"]);

    let logout = app
        .with_token(Method::POST, "/v1/auth/logout", &second_token)
        .await;
    assert_eq!(logout.status, StatusCode::NO_CONTENT);
    assert_eq!(logout.body, Value::Null);
    assert_error(
        &app.me(&second_token).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
    );
    assert_eq!(
        app.me(&first_token).await.status,
        StatusCode::OK,
        "logout ends only its own session"
    );
}

// ==============================================================================
// Register and login failures
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn register_rejects_a_taken_username(pool: PgPool) {
    let app = TestApp::new(pool);
    app.register("alice").await;
    let again = app
        .post_auth("/v1/auth/register", "Alice", "another password")
        .await;
    assert_error(&again, StatusCode::CONFLICT, "username_taken");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn register_respects_allow_registration(pool: PgPool) {
    let app = TestApp::with_registration(pool, false);
    let response = app.post_auth("/v1/auth/register", "alice", PASSWORD).await;
    assert_error(&response, StatusCode::FORBIDDEN, "registration_disabled");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn register_validates_input(pool: PgPool) {
    let app = TestApp::new(pool);
    for (username, password) in [("al ice", PASSWORD), ("alice", "short")] {
        let response = app.post_auth("/v1/auth/register", username, password).await;
        assert_error(&response, StatusCode::BAD_REQUEST, "bad_request");
    }
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn wrong_password_and_unknown_user_look_the_same(pool: PgPool) {
    let app = TestApp::new(pool);
    app.register("alice").await;
    let wrong_password = app
        .post_auth("/v1/auth/login", "alice", "not the password")
        .await;
    let unknown_user = app.post_auth("/v1/auth/login", "bob", PASSWORD).await;
    let invalid_username = app
        .post_auth("/v1/auth/login", "not a username!", PASSWORD)
        .await;
    assert_error(
        &wrong_password,
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
    );
    assert_eq!(unknown_user.body, wrong_password.body);
    assert_eq!(invalid_username.body, wrong_password.body);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn malformed_bodies_use_the_error_shape(pool: PgPool) {
    let app = TestApp::new(pool);
    let requests = [
        Request::post("/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not json")),
        Request::post("/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username": "alice"}"#)),
        Request::post("/v1/auth/register").body(Body::from(
            r#"{"username": "alice", "password": "12345678", "device_name": "x"}"#,
        )),
    ];
    for request in requests {
        let response = app.send(request.expect("valid request")).await;
        assert_error(&response, StatusCode::BAD_REQUEST, "bad_request");
    }
}

// ==============================================================================
// Bearer tokens and expiry
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn bad_bearer_tokens_are_unauthenticated(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.register("alice").await;
    let unknown = "A".repeat(43);
    let authorizations = [
        None,
        Some(format!("Basic {token}")),
        Some("Bearer garbled!".to_owned()),
        Some(format!("Bearer {unknown}")),
        Some(format!("Bearer {token}x")),
    ];
    for authorization in authorizations {
        let mut request = Request::get("/v1/auth/me");
        if let Some(value) = &authorization {
            request = request.header(header::AUTHORIZATION, value);
        }
        let response = app
            .send(request.body(Body::empty()).expect("valid request"))
            .await;
        assert_error(&response, StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    let lowercase_scheme = app
        .send(
            Request::get("/v1/auth/me")
                .header(header::AUTHORIZATION, format!("bearer {token}"))
                .body(Body::empty())
                .expect("valid request"),
        )
        .await;
    assert_eq!(lowercase_scheme.status, StatusCode::OK);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn expired_token_is_unauthenticated(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.register("alice").await;
    app.advance_ms(90 * DAY_MS);
    assert_error(
        &app.me(&token).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
    );
    assert_error(
        &app.with_token(Method::POST, "/v1/auth/logout", &token)
            .await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn expiry_slides_only_after_24_hours(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.register("alice").await;
    let registered_at = app.now();
    let original_expiry = plus_ms(registered_at, 90 * DAY_MS);
    assert_eq!(
        app.session_times(&token).await,
        (registered_at, original_expiry)
    );

    app.advance_ms(24 * HOUR_MS);
    assert_eq!(app.me(&token).await.status, StatusCode::OK);
    assert_eq!(
        app.session_times(&token).await,
        (app.now(), original_expiry),
        "24 h is not more than 24 h, so only last_seen moves"
    );

    app.advance_ms(1);
    assert_eq!(app.me(&token).await.status, StatusCode::OK);
    let extended_at = app.now();
    assert_eq!(
        app.session_times(&token).await,
        (extended_at, plus_ms(extended_at, 90 * DAY_MS))
    );

    // The extension counts from now, so the token outlives its original
    // expiry.
    app.advance_ms(90 * DAY_MS - 1);
    assert_eq!(app.me(&token).await.status, StatusCode::OK);
}

// ==============================================================================
// Rate limits
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn login_is_rate_limited_per_username(pool: PgPool) {
    let app = TestApp::new(pool);
    for attempt in 0..10 {
        let ip = format!("198.51.100.{attempt}");
        let response = app
            .post_auth_from("/v1/auth/login", "mallory", "guess", Some(&ip))
            .await;
        assert_error(&response, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    let limited = app
        .post_auth_from("/v1/auth/login", "Mallory", "guess", Some("198.51.100.99"))
        .await;
    assert_rate_limited(&limited);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn login_and_register_share_the_limit_per_ip(pool: PgPool) {
    let app = TestApp::new(pool);
    for attempt in 0..9 {
        let response = app
            .post_auth("/v1/auth/login", &format!("user{attempt}"), "guess")
            .await;
        assert_error(&response, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    app.register("alice").await;
    let limited = app.post_auth("/v1/auth/register", "bob", PASSWORD).await;
    assert_rate_limited(&limited);
    let other_ip = app
        .post_auth_from("/v1/auth/register", "bob", PASSWORD, Some("203.0.113.7"))
        .await;
    assert_eq!(other_ip.status, StatusCode::OK, "{}", other_ip.body);
}

// ==============================================================================
// Client IP behind a reverse proxy
// ==============================================================================

/// Spends one IP's whole budget with distinct usernames, so only the IP key
/// can refuse the next attempt.
async fn exhaust_ip_budget(app: &TestApp, client_ip: Option<&str>) {
    for attempt in 0..10 {
        let response = app
            .post_auth_from(
                "/v1/auth/login",
                &format!("user{attempt}"),
                "guess",
                client_ip,
            )
            .await;
        assert_error(&response, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn clients_behind_a_trusted_proxy_get_separate_buckets(pool: PgPool) {
    let app = TestApp::new(pool);
    exhaust_ip_budget(&app, Some("198.51.100.1")).await;
    let same_client = app
        .post_auth_from("/v1/auth/login", "other", "guess", Some("198.51.100.1"))
        .await;
    assert_rate_limited(&same_client);
    let other_client = app
        .post_auth_from("/v1/auth/login", "other", "guess", Some("198.51.100.2"))
        .await;
    assert_error(
        &other_client,
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn client_ip_header_from_an_untrusted_peer_is_ignored(pool: PgPool) {
    let app = TestApp::behind_untrusted_peer(pool);
    exhaust_ip_budget(&app, Some("198.51.100.1")).await;
    let forged = app
        .post_auth_from("/v1/auth/login", "other", "guess", Some("198.51.100.2"))
        .await;
    assert_rate_limited(&forged);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn garbage_client_ip_header_falls_back_to_the_peer(pool: PgPool) {
    let app = TestApp::new(pool);
    for garbage in ["not-an-ip", "198.51.100.1, 198.51.100.2", ""] {
        let response = app
            .post_auth_from("/v1/auth/login", garbage, "guess", Some(garbage))
            .await;
        assert_error(&response, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    // A valid header naming the peer's own address reaches the peer's bucket
    // without taking the fallback path.
    for attempt in 3..10 {
        let response = app
            .post_auth_from(
                "/v1/auth/login",
                &format!("peer{attempt}"),
                "guess",
                Some(TRUSTED_PEER),
            )
            .await;
        assert_error(&response, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    let limited = app
        .post_auth_from("/v1/auth/login", "peer10", "guess", Some("not-an-ip"))
        .await;
    assert_rate_limited(&limited);
}

fn assert_rate_limited(response: &TestResponse) {
    assert_error(response, StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    let retry_after: u64 = response.headers[header::RETRY_AFTER]
        .to_str()
        .expect("ASCII header")
        .parse()
        .expect("whole seconds");
    assert!((1..=60).contains(&retry_after), "{retry_after}");
}
