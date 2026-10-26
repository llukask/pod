//! An in-process router over a test database with a hand-moved clock, for
//! the `/v1/auth/*` test binaries.

// Each test binary uses a different subset.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use pod_protocol::Timestamp;
use pod_server::AppState;
use pod_server::clock::Clock;
use pod_server::db::SqlTimestamp;
use pod_server::directory::Directory;
use pod_server::fetch::{Allowlist, FetchPolicy};
use pod_server::http::ClientIpSource;
use pod_server::http::rate_limit::RateLimiter;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;
use url::Url;

pub const HOUR_MS: i64 = 60 * 60 * 1000;
pub const DAY_MS: i64 = 24 * HOUR_MS;
pub const PASSWORD: &str = "correct horse battery";
pub const TRUSTED_PEER: &str = "192.0.2.1";
pub const UNTRUSTED_PEER: &str = "192.0.2.2";

/// The router over a test database, with a clock the test moves by hand.
pub struct TestApp {
    router: Router,
    pub pool: PgPool,
    now: Arc<Mutex<Timestamp>>,
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

impl TestApp {
    pub fn new(pool: PgPool) -> Self {
        Self::with_registration(pool, true)
    }

    /// Every request comes from the mocked peer [`TRUSTED_PEER`], a trusted
    /// proxy, so tests can vary the client IP per request with `X-Real-IP`.
    pub fn with_registration(pool: PgPool, allow_registration: bool) -> Self {
        Self::build(
            pool,
            allow_registration,
            "https://itunes.apple.com",
            TRUSTED_PEER,
        )
    }

    /// Sends directory searches to the stub at `directory_base_url`.
    pub fn with_directory(pool: PgPool, directory_base_url: &str) -> Self {
        Self::build(pool, true, directory_base_url, TRUSTED_PEER)
    }

    /// Every request comes from [`UNTRUSTED_PEER`], whose `X-Real-IP` the
    /// server ignores.
    pub fn behind_untrusted_peer(pool: PgPool) -> Self {
        Self::build(pool, true, "https://itunes.apple.com", UNTRUSTED_PEER)
    }

    fn build(pool: PgPool, allow_registration: bool, directory_base_url: &str, peer: &str) -> Self {
        let directory = Directory::new(
            &Url::parse(directory_base_url).expect("tests pass valid URLs"),
            "https://pod.example.org",
        )
        .expect("the directory client builds");
        let now = Arc::new(Mutex::new(Timestamp::now()));
        let clock_now = Arc::clone(&now);
        let state = AppState {
            pool: pool.clone(),
            allow_registration,
            client_ip: Arc::new(
                ClientIpSource::parse(Some("X-Real-IP"), &[TRUSTED_PEER])
                    .expect("valid header and address"),
            ),
            auth_rate_limit: Arc::new(RateLimiter::auth()),
            clock: Clock::from_fn(move || *clock_now.lock().expect("not poisoned")),
            directory: Arc::new(directory),
            fetch_policy: FetchPolicy::new(Allowlist::default()),
        };
        let peer = SocketAddr::new(peer.parse().expect("tests pass valid IPs"), 50000);
        let router = pod_server::app(state).layer(MockConnectInfo(peer));
        Self { router, pool, now }
    }

    pub fn now(&self) -> Timestamp {
        *self.now.lock().expect("not poisoned")
    }

    pub fn advance_ms(&self, ms: i64) {
        let mut now = self.now.lock().expect("not poisoned");
        *now = Timestamp::from_epoch_ms(now.as_epoch_ms() + ms).expect("tests stay near today");
    }

    pub async fn send(&self, request: Request<Body>) -> TestResponse {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("the router is infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("the body is in memory");
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("non-JSON body: {}", String::from_utf8_lossy(&bytes)))
        };
        TestResponse {
            status,
            headers,
            body,
        }
    }

    pub async fn post_auth(&self, path: &str, username: &str, password: &str) -> TestResponse {
        self.post_auth_from(path, username, password, None).await
    }

    /// Sends `client_ip` as `X-Real-IP`, verbatim so tests can send garbage.
    pub async fn post_auth_from(
        &self,
        path: &str,
        username: &str,
        password: &str,
        client_ip: Option<&str>,
    ) -> TestResponse {
        let mut request = Request::post(path).header(header::CONTENT_TYPE, "application/json");
        if let Some(ip) = client_ip {
            request = request.header("x-real-ip", ip);
        }
        let body = json!({ "username": username, "password": password, "device_name": "Pixel 8" });
        self.send(
            request
                .body(Body::from(body.to_string()))
                .expect("valid request"),
        )
        .await
    }

    pub async fn register(&self, username: &str) -> String {
        let response = self
            .post_auth("/v1/auth/register", username, PASSWORD)
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        token_of(&response)
    }

    pub async fn with_token(&self, method: Method, path: &str, token: &str) -> TestResponse {
        self.send(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
    }

    pub async fn me(&self, token: &str) -> TestResponse {
        self.with_token(Method::GET, "/v1/auth/me", token).await
    }

    pub async fn session_times(&self, token: &str) -> (Timestamp, Timestamp) {
        let row = sqlx::query!(
            r#"select last_seen as "last_seen: SqlTimestamp",
                      expires_at as "expires_at: SqlTimestamp"
               from session where token_hash = $1"#,
            &Sha256::digest(token.as_bytes())[..]
        )
        .fetch_one(&self.pool)
        .await
        .expect("the session exists");
        (row.last_seen.0, row.expires_at.0)
    }
}

pub fn token_of(response: &TestResponse) -> String {
    response.body["token"]
        .as_str()
        .expect("token responses carry a token")
        .to_owned()
}

pub fn plus_ms(at: Timestamp, ms: i64) -> Timestamp {
    Timestamp::from_epoch_ms(at.as_epoch_ms() + ms).expect("tests stay near today")
}

pub fn assert_error(response: &TestResponse, status: StatusCode, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(response.body["error"]["code"], code, "{}", response.body);
    assert!(
        response.body["error"]["message"].is_string(),
        "{}",
        response.body
    );
}
