//! The `pod-server admin` commands, called the way `main` calls them but
//! writing into a buffer instead of stdout.

mod auth_app;
mod common;

use auth_app::{PASSWORD, TestApp, TestResponse};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use pod_protocol::{EpisodeId, MutationId, PodcastId, Timestamp, normalize_feed_url};
use pod_server::admin;
use pod_server::db::SqlTimestamp;
use pod_server::feed::ParsedFeed;
use pod_server::ingest::ingest;
use serde_json::{Value, json};
use sqlx::PgPool;

const FEED_URL: &str = "https://feeds.example.com/daily.xml";
const NEW_PASSWORD: &str = "a much longer passphrase";

/// What a command wrote, as text.
async fn output<F>(command: impl FnOnce(Vec<u8>) -> F) -> String
where
    F: Future<Output = Vec<u8>>,
{
    String::from_utf8(command(Vec::new()).await).expect("commands write UTF-8")
}

// ==============================================================================
// purge-episode
// ==============================================================================

/// A user subscribed (through sync) to a podcast whose feed has episodes
/// `a`, `b` and `c`, ingested.
struct Subscribed {
    app: TestApp,
    token: String,
    podcast_id: PodcastId,
    feed: ParsedFeed,
}

async fn subscribed(pool: PgPool) -> Subscribed {
    let app = TestApp::new(pool);
    let token = app.register("alice").await;
    let url = normalize_feed_url(FEED_URL).expect("the feed URL is valid");
    let podcast_id = PodcastId::from_feed_url(&url);
    let subscribe = json!({
        "id": MutationId::new_v7(), "at": app.now(),
        "dev": "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d",
        "type": "subscription.set", "podcast_id": podcast_id, "feed_url": FEED_URL,
        "subscribed": true,
    });
    let response = sync(&app, &token, json!({ "mutations": [subscribe] })).await;
    assert_eq!(
        response.body["results"][0]["status"], "applied",
        "{}",
        response.body
    );

    let feed = common::parsed_feed(
        "The Daily",
        vec![
            common::parsed_episode(podcast_id, "c", 3),
            common::parsed_episode(podcast_id, "b", 2),
            common::parsed_episode(podcast_id, "a", 1),
        ],
    );
    ingest(&app.pool, podcast_id, &feed)
        .await
        .expect("the ingest runs")
        .expect("subscribe created the podcast");
    Subscribed {
        app,
        token,
        podcast_id,
        feed,
    }
}

async fn sync(app: &TestApp, token: &str, body: Value) -> TestResponse {
    let response = app
        .send(
            Request::post("/v1/sync")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .expect("valid request"),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    response
}

impl Subscribed {
    /// Pulls the podcast scope from `cursor` and returns its complete
    /// changes, `{version, episodes, ...}`.
    async fn pull(&self, cursor: i64) -> Value {
        let response = sync(
            &self.app,
            &self.token,
            json!({
                "mutations": [],
                "scopes": { "user": 0, "podcasts": [{ "id": self.podcast_id, "version": cursor }] },
                "limit": 500,
            }),
        )
        .await;
        let scope = response.body["scopes"]["podcasts"][0].clone();
        assert_eq!(
            (&scope["status"], &scope["complete"]),
            (&json!("ok"), &json!(true))
        );
        scope
    }

    async fn purge(&self, episode_id: EpisodeId) -> String {
        output(async |mut out| {
            admin::purge_episode(&self.app.pool, episode_id, &mut out)
                .await
                .expect("the purge runs");
            out
        })
        .await
    }
}

fn cursor(scope: &Value) -> i64 {
    scope["version"].as_i64().expect("a scope has a version")
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_purged_episode_reaches_the_next_pull_as_a_tombstone(pool: PgPool) {
    let s = subscribed(pool).await;
    let synced = cursor(&s.pull(0).await);
    let purged = EpisodeId::new(s.podcast_id, "b");

    let printed = s.purge(purged).await;

    assert_eq!(printed, "purged (version 5)\n");
    let scope = s.pull(synced).await;
    assert_eq!(
        scope["episodes"],
        json!([{ "id": purged, "deleted": true, "version": 5 }])
    );
    assert_eq!(cursor(&scope), 5);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn purging_twice_reports_already_deleted(pool: PgPool) {
    let s = subscribed(pool).await;
    let purged = EpisodeId::new(s.podcast_id, "b");
    s.purge(purged).await;

    assert_eq!(s.purge(purged).await, "already deleted\n");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn purging_an_unknown_episode_fails(pool: PgPool) {
    let s = subscribed(pool).await;
    let unknown = EpisodeId::new(s.podcast_id, "never-published");

    let error = admin::purge_episode(&s.app.pool, unknown, &mut Vec::new())
        .await
        .expect_err("there is no such episode");

    assert_eq!(error.to_string(), format!("no such episode {unknown}"));
}

/// Purge wins until the publisher changes the episode: an ingest of a feed
/// that still carries it unchanged, next to other changes, leaves it out.
#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_next_ingest_does_not_resurrect_a_purged_episode(pool: PgPool) {
    let mut s = subscribed(pool).await;
    let purged = EpisodeId::new(s.podcast_id, "b");
    s.purge(purged).await;
    let synced = cursor(&s.pull(0).await);

    s.feed.episodes[0].title = "Corrected title".to_owned();
    ingest(&s.app.pool, s.podcast_id, &s.feed)
        .await
        .expect("the ingest runs")
        .expect("the podcast exists");

    let scope = s.pull(synced).await;
    let changed: Vec<(&Value, &Value)> = scope["episodes"]
        .as_array()
        .expect("episodes is a list")
        .iter()
        .map(|episode| (&episode["id"], &episode["deleted"]))
        .collect();
    assert_eq!(
        changed,
        [(&json!(EpisodeId::new(s.podcast_id, "c")), &json!(false))]
    );
    let backfill = s.pull(0).await;
    let b = backfill["episodes"]
        .as_array()
        .expect("episodes is a list")
        .iter()
        .find(|episode| episode["id"] == json!(purged))
        .expect("a backfill carries the purged episode");
    assert_eq!(b["deleted"], json!(true), "{b}");
}

// ==============================================================================
// list-users
// ==============================================================================

async fn user(pool: &PgPool, id: &str, username: &str, created_at: Timestamp) {
    sqlx::query!(
        "insert into users (id, username, password_hash, created_at)
         values ($1::text::uuid, $2, '!', $3)",
        id,
        username,
        SqlTimestamp(created_at) as _
    )
    .execute(pool)
    .await
    .expect("the user inserts");
}

async fn session(pool: &PgPool, user_id: &str, device_name: &str, expires_at: Timestamp) {
    sqlx::query!(
        "insert into session (token_hash, user_id, device_id, device_name, expires_at)
         values (sha256($1::text::bytea), $2::text::uuid, gen_random_uuid(), $1, $3)",
        device_name,
        user_id,
        SqlTimestamp(expires_at) as _
    )
    .execute(pool)
    .await
    .expect("the session inserts");
}

fn ts(text: &str) -> Timestamp {
    text.parse().expect("a valid timestamp literal")
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn list_users_shows_live_sessions_by_username(pool: PgPool) {
    let now = ts("2026-10-25T12:00:00Z");
    let bob = "019a3f10-0000-7000-8000-00000000000b";
    let alice = "019a3f10-0000-7000-8000-00000000000a";
    user(&pool, bob, "bob", ts("2026-02-01T08:30:00Z")).await;
    user(&pool, alice, "alice", ts("2026-03-01T09:00:00.250Z")).await;
    session(&pool, alice, "phone", ts("2026-12-01T00:00:00Z")).await;
    session(&pool, alice, "laptop", ts("2026-11-01T00:00:00Z")).await;
    session(&pool, bob, "old tablet", now).await;

    let printed = output(async |mut out| {
        admin::list_users(&pool, now, &mut out)
            .await
            .expect("the listing runs");
        out
    })
    .await;

    assert_eq!(
        printed,
        format!(
            "id\tusername\tcreated_at\tsessions\n\
             {alice}\talice\t2026-03-01T09:00:00.250Z\t2\n\
             {bob}\tbob\t2026-02-01T08:30:00.000Z\t0\n"
        )
    );
}

// ==============================================================================
// reset-password
// ==============================================================================

async fn reset(app: &TestApp, username: &str, password: &str) -> color_eyre::Result<String> {
    let mut out = Vec::new();
    admin::reset_password(&app.pool, username, password.to_owned(), &mut out).await?;
    Ok(String::from_utf8(out).expect("commands write UTF-8"))
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn reset_password_replaces_the_hash_and_signs_out_every_device(pool: PgPool) {
    let app = TestApp::new(pool);
    let first = app.register("alice").await;
    let response = app.post_auth("/v1/auth/login", "alice", PASSWORD).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);

    let printed = reset(&app, "Alice", NEW_PASSWORD)
        .await
        .expect("the reset runs");

    assert_eq!(
        printed,
        "reset the password of alice and signed out 2 sessions; every device must log in again\n"
    );
    let me = app.me(&first).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    let old = app.post_auth("/v1/auth/login", "alice", PASSWORD).await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    let new = app.post_auth("/v1/auth/login", "alice", NEW_PASSWORD).await;
    assert_eq!(new.status, StatusCode::OK, "{}", new.body);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn reset_password_rejects_an_unknown_user(pool: PgPool) {
    let app = TestApp::new(pool);

    let error = reset(&app, "mallory", NEW_PASSWORD)
        .await
        .expect_err("there is no such user");

    assert_eq!(error.to_string(), "no such user mallory");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn reset_password_holds_the_password_to_the_register_rules(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.register("alice").await;

    let error = reset(&app, "alice", "short")
        .await
        .expect_err("the password is too short");

    assert_eq!(
        error.to_string(),
        "Password must be at least 8 characters and at most 1024 bytes long."
    );
    let me = app.me(&token).await;
    assert_eq!(
        me.status,
        StatusCode::OK,
        "a rejected reset signs no one out"
    );
}
