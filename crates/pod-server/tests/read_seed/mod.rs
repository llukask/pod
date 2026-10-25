//! A signed-in user and direct-SQL seeds for the read endpoint tests.
//!
//! The read endpoints only read, so these seeds write rows as plainly as
//! the schema allows: versions come from the scope owner's counter, but
//! without the scope lock, and episodes carry only the required columns.

// Each test binary uses a different subset.
#![allow(dead_code)]

use axum::http::{Method, StatusCode};
use pod_protocol::{EpisodeId, PodcastId, Timestamp, UserId};
use pod_server::db::SqlTimestamp;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgPool;

use crate::auth_app::{self, PASSWORD, TestApp, TestResponse};

/// A fixed device id for every stamp the seeds write.
const DEVICE: &str = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d";

/// The `*_at` of every episode state field the seeds set. Read endpoints
/// don't look at stamps.
const STATE_AT_MS: i64 = 1_767_225_600_000;

/// A registered user with their bearer token.
pub struct Reader {
    pub app: TestApp,
    pub token: String,
    pub user_id: UserId,
}

pub async fn reader(pool: PgPool) -> Reader {
    let app = TestApp::new(pool);
    let response = app.post_auth("/v1/auth/register", "alice", PASSWORD).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let token = auth_app::token_of(&response);
    let user_id = serde_json::from_value(response.body["user_id"].clone())
        .expect("register returns the user id");
    Reader {
        app,
        token,
        user_id,
    }
}

impl Reader {
    pub fn pool(&self) -> &PgPool {
        &self.app.pool
    }

    /// Moves the server's clock to `now`.
    pub fn set_now(&self, now: Timestamp) {
        self.app
            .advance_ms(now.as_epoch_ms() - self.app.now().as_epoch_ms());
    }

    pub async fn get(&self, path: &str) -> TestResponse {
        self.app.with_token(Method::GET, path, &self.token).await
    }

    /// Asserts a 200 and parses the body, checking that the protocol type
    /// serializes back to exactly the wire body.
    pub async fn get_ok<T: DeserializeOwned + Serialize>(&self, path: &str) -> T {
        let response = self.get(path).await;
        assert_eq!(response.status, StatusCode::OK, "{path}: {}", response.body);
        let parsed: T = serde_json::from_value(response.body.clone())
            .unwrap_or_else(|error| panic!("{path}: {error}: {}", response.body));
        assert_eq!(
            serde_json::to_value(&parsed).expect("protocol types serialize"),
            response.body,
            "{path}: the wire body is exactly what the protocol types produce"
        );
        parsed
    }

    pub async fn subscribe(&self, podcast_id: PodcastId, subscribed_at: Timestamp) {
        set_subscription(self.pool(), self.user_id, podcast_id, true, subscribed_at).await;
    }

    pub async fn unsubscribe(&self, podcast_id: PodcastId, at: Timestamp) {
        set_subscription(self.pool(), self.user_id, podcast_id, false, at).await;
    }
}

/// The ids in a JSON page's `items`.
pub fn item_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("pages have items")
        .iter()
        .map(|item| item["id"].as_str().expect("items have ids").to_owned())
        .collect()
}

// ==============================================================================
// Catalog
// ==============================================================================

/// Inserts an ingested podcast (`ok`, at version 1) with a title and artwork.
pub async fn insert_podcast(pool: &PgPool, podcast_id: PodcastId, title: &str) {
    sqlx::query!(
        "insert into podcast (id, feed_url, status, title, artwork_url, version, version_counter)
         values ($1, $2, 'ok', $3, $4, 1, 1)",
        podcast_id.as_uuid(),
        format!("https://feeds.example.com/{podcast_id}.xml"),
        title,
        format!("https://images.example.com/{podcast_id}.jpg"),
    )
    .execute(pool)
    .await
    .expect("the podcast is new");
}

/// Inserts an episode keyed by its id, with the next podcast-scope version.
pub async fn insert_episode(
    pool: &PgPool,
    podcast_id: PodcastId,
    episode_id: EpisodeId,
    published_at: Timestamp,
    deleted: bool,
) {
    let guid = episode_id.to_string();
    sqlx::query!(
        "with counter as (
             update podcast set version_counter = version_counter + 1 where id = $2
             returning version_counter
         )
         insert into episode (id, podcast_id, guid, title, published_at, audio_url, audio_type,
             content_hash, deleted, version)
         select $1, $2, $3, $4, $5, $6, 'audio/mpeg', $7, $8, version_counter from counter",
        episode_id.as_uuid(),
        podcast_id.as_uuid(),
        guid,
        format!("Episode {guid}"),
        SqlTimestamp(published_at) as _,
        format!("https://media.example.com/{guid}.mp3"),
        guid.as_bytes(),
        deleted
    )
    .execute(pool)
    .await
    .expect("the episode is new");
}

// ==============================================================================
// User scope
// ==============================================================================

pub async fn set_subscription(
    pool: &PgPool,
    user_id: UserId,
    podcast_id: PodcastId,
    subscribed: bool,
    subscribed_at: Timestamp,
) {
    sqlx::query!(
        "with counter as (
             update users set version_counter = version_counter + 1 where id = $1
             returning version_counter
         )
         insert into subscription
             (user_id, podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev, version)
         select $1, $2, $3, $4, $5, $6, version_counter from counter
         on conflict (user_id, podcast_id) do update set
             subscribed = excluded.subscribed, subscribed_at = excluded.subscribed_at,
             version = excluded.version",
        user_id.as_uuid(),
        podcast_id.as_uuid(),
        format!("https://feeds.example.com/{podcast_id}.xml"),
        subscribed,
        SqlTimestamp(subscribed_at) as _,
        device_uuid(),
    )
    .execute(pool)
    .await
    .expect("the user exists");
}

/// Writes the user's state for an episode, stamping only the fields that
/// differ from the defaults, as a client's mutations would.
pub async fn set_episode_state(
    pool: &PgPool,
    user_id: UserId,
    podcast_id: PodcastId,
    episode_id: EpisodeId,
    position_ms: i64,
    played: bool,
    archived: bool,
) {
    let at = Timestamp::from_epoch_ms(STATE_AT_MS).expect("2026 is in range");
    let stamp = |set: bool| set.then_some(SqlTimestamp(at));
    let dev = |set: bool| set.then_some(device_uuid());
    sqlx::query!(
        "with counter as (
             update users set version_counter = version_counter + 1 where id = $1
             returning version_counter
         )
         insert into episode_state
             (user_id, episode_id, podcast_id, position_ms, position_at, position_dev,
              played, played_at, played_dev, archived, archived_at, archived_dev, version)
         select $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, version_counter from counter",
        user_id.as_uuid(),
        episode_id.as_uuid(),
        podcast_id.as_uuid(),
        position_ms,
        stamp(position_ms != 0) as _,
        dev(position_ms != 0),
        played,
        stamp(played) as _,
        dev(played),
        archived,
        stamp(archived) as _,
        dev(archived),
    )
    .execute(pool)
    .await
    .expect("the episode exists and has no state yet");
}

pub async fn set_setting(pool: &PgPool, user_id: UserId, key: &str, value: Value, at: Timestamp) {
    sqlx::query!(
        "with counter as (
             update users set version_counter = version_counter + 1 where id = $1
             returning version_counter
         )
         insert into user_setting (user_id, key, value, value_at, value_dev, version)
         select $1, $2, $3, $4, $5, version_counter from counter",
        user_id.as_uuid(),
        key,
        value,
        SqlTimestamp(at) as _,
        device_uuid(),
    )
    .execute(pool)
    .await
    .expect("the setting is new");
}

fn device_uuid() -> sqlx::types::Uuid {
    DEVICE.parse().expect("literal is a UUID")
}
