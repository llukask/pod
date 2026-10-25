//! Secrets never reach the logs or the database in plain text.
//!
//! This is its own test binary because the capture lost events while other
//! tests ran in the same process, most likely through tracing's global
//! per-callsite interest cache.

mod auth_app;

use std::io::Write;
use std::sync::{Arc, Mutex};

use auth_app::{PASSWORD, TestApp, token_of};
use axum::http::{Method, StatusCode};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::instrument::WithSubscriber;

/// Collects everything the fmt subscriber writes.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("not poisoned")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn tokens_and_passwords_are_never_stored_or_logged(pool: PgPool) {
    let logs = CapturedLogs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let app = TestApp::new(pool.clone());
    let (registered, logged_in) = async {
        let registered = app.register("alice").await;
        let logged_in = token_of(&app.post_auth("/v1/auth/login", "alice", PASSWORD).await);
        assert_eq!(app.me(&logged_in).await.status, StatusCode::OK);
        let wrong = app
            .post_auth("/v1/auth/login", "alice", "hunter2hunter2")
            .await;
        assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
        let logout = app
            .with_token(Method::POST, "/v1/auth/logout", &logged_in)
            .await;
        assert_eq!(logout.status, StatusCode::NO_CONTENT);
        (registered, logged_in)
    }
    .with_subscriber(subscriber)
    .await;

    let logs =
        String::from_utf8(logs.0.lock().expect("not poisoned").clone()).expect("logs are UTF-8");
    assert!(logs.contains("registered"), "the capture works: {logs}");
    for secret in [registered.as_str(), &logged_in, PASSWORD, "hunter2hunter2"] {
        assert!(!logs.contains(secret), "{secret:?} leaked into the logs");
    }

    let sessions = sqlx::query!("select token_hash, device_name from session")
        .fetch_all(&pool)
        .await
        .expect("the session table is readable");
    assert_eq!(sessions.len(), 1, "logout deleted the second session");
    assert_eq!(
        sessions[0].token_hash,
        Sha256::digest(registered.as_bytes()).to_vec()
    );
    let password_hash = sqlx::query_scalar!("select password_hash from users")
        .fetch_one(&pool)
        .await
        .expect("one user");
    assert!(
        password_hash.starts_with("$argon2id$v=19$"),
        "{password_hash}"
    );
    assert!(!password_hash.contains(PASSWORD));
}
