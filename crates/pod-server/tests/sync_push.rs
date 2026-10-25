//! The push half of `POST /v1/sync` (spec 4.2 to 4.4): mutations applied
//! under last-writer-wins, replays, rejections and the user-scope lock.

mod auth_app;
mod common;

use std::path::Path;

use auth_app::{PASSWORD, TestApp, TestResponse, assert_error, plus_ms};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use pod_protocol::{
    DeviceId, EpisodeId, EpisodePlayed, EpisodePosition, EpisodeState, Mutation, MutationId,
    MutationOutcome, PodcastId, Timestamp, UserId,
};
use pod_server::db::SqlTimestamp;
use pod_server::fetch::{Allowlist, FetchPolicy};
use pod_server::sync::apply_batch;
use pod_server::version::VersionScope;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;

// ==============================================================================
// Helpers
// ==============================================================================

/// A registered user with one podcast of three episodes.
struct Fixture {
    app: TestApp,
    token: String,
    user_id: UserId,
    podcast_id: PodcastId,
    episodes: Vec<EpisodeId>,
}

async fn fixture(pool: PgPool) -> Fixture {
    let app = TestApp::new(pool);
    let response = app.post_auth("/v1/auth/register", "alice", PASSWORD).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let token = auth_app::token_of(&response);
    let user_id: UserId = serde_json::from_value(response.body["user_id"].clone())
        .expect("register returns the user id");
    let podcast_id = common::create_podcast(&app.pool, "https://example.com/feed.xml").await;
    let episodes = common::create_episodes(&app.pool, podcast_id, 3).await;
    Fixture {
        app,
        token,
        user_id,
        podcast_id,
        episodes,
    }
}

impl Fixture {
    async fn sync(&self, body: Value) -> TestResponse {
        self.sync_raw(body.to_string()).await
    }

    async fn sync_raw(&self, body: String) -> TestResponse {
        self.app
            .send(
                Request::post("/v1/sync")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
    }

    /// Pushes `mutations` and returns the results, asserting a 200.
    async fn push(&self, mutations: &[Value]) -> Vec<Value> {
        let response = self.sync(json!({ "mutations": mutations })).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        response.body["results"]
            .as_array()
            .expect("a sync response has results")
            .clone()
    }

    fn ago(&self, ms: i64) -> Timestamp {
        plus_ms(self.app.now(), -ms)
    }

    async fn state(&self, episode_id: EpisodeId) -> Option<EpisodeState> {
        episode_state(&self.app.pool, self.user_id, episode_id).await
    }

    async fn version_counter(&self) -> i64 {
        version_counter(&self.app.pool, self.user_id).await
    }
}

fn position(episode_id: EpisodeId, position_ms: i64, at: Timestamp, dev: DeviceId) -> Value {
    position_with_id(MutationId::new_v7(), episode_id, position_ms, at, dev)
}

fn position_with_id(
    id: MutationId,
    episode_id: EpisodeId,
    position_ms: i64,
    at: Timestamp,
    dev: DeviceId,
) -> Value {
    json!({
        "id": id, "at": at, "dev": dev, "type": "episode.position",
        "episode_id": episode_id, "position_ms": position_ms,
    })
}

fn played(episode_id: EpisodeId, played: bool, at: Timestamp, dev: DeviceId) -> Value {
    json!({
        "id": MutationId::new_v7(), "at": at, "dev": dev, "type": "episode.played",
        "episode_id": episode_id, "played": played,
    })
}

fn archived(episode_id: EpisodeId, archived: bool, at: Timestamp, dev: DeviceId) -> Value {
    json!({
        "id": MutationId::new_v7(), "at": at, "dev": dev, "type": "episode.archived",
        "episode_id": episode_id, "archived": archived,
    })
}

fn setting(key: &str, value: Value, at: Timestamp, dev: DeviceId) -> Value {
    json!({
        "id": MutationId::new_v7(), "at": at, "dev": dev, "type": "setting.set",
        "key": key, "value": value,
    })
}

/// Two device ids with a known order.
fn devices() -> (DeviceId, DeviceId) {
    let smaller: DeviceId = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d"
        .parse()
        .expect("literal is a UUID");
    let greater: DeviceId = "019b2e44-1f0a-7c55-8d21-3a4b5c6d7e8f"
        .parse()
        .expect("literal is a UUID");
    (smaller, greater)
}

fn assert_result(result: &Value, mutation: &Value, status: &str, at: Timestamp) {
    assert_eq!(result["id"], mutation["id"], "{result}");
    assert_eq!(result["status"], status, "{result}");
    assert_eq!(result["at"], json!(at), "{result}");
}

async fn episode_state(
    pool: &PgPool,
    user_id: UserId,
    episode_id: EpisodeId,
) -> Option<EpisodeState> {
    let row = sqlx::query!(
        r#"select podcast_id, position_ms, position_at as "position_at: SqlTimestamp",
                  position_dev, played, played_at as "played_at: SqlTimestamp", played_dev,
                  archived, archived_at as "archived_at: SqlTimestamp", archived_dev, version
           from episode_state where user_id = $1 and episode_id = $2"#,
        user_id.as_uuid(),
        episode_id.as_uuid()
    )
    .fetch_optional(pool)
    .await
    .expect("episode_state is readable")?;
    Some(EpisodeState {
        episode_id,
        podcast_id: PodcastId::from_uuid(row.podcast_id),
        position_ms: row.position_ms,
        position_at: row.position_at.map(Timestamp::from),
        position_dev: row.position_dev.map(DeviceId::from_uuid),
        played: row.played,
        played_at: row.played_at.map(Timestamp::from),
        played_dev: row.played_dev.map(DeviceId::from_uuid),
        archived: row.archived,
        archived_at: row.archived_at.map(Timestamp::from),
        archived_dev: row.archived_dev.map(DeviceId::from_uuid),
        version: row.version,
    })
}

async fn version_counter(pool: &PgPool, user_id: UserId) -> i64 {
    sqlx::query_scalar!(
        "select version_counter from users where id = $1",
        user_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("the user exists")
}

// ==============================================================================
// Last-writer-wins
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_later_at_applies_and_an_earlier_one_is_superseded(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();
    let episode = f.episodes[0];

    let first = position(episode, 60_000, f.ago(2_000), phone);
    let later = position(episode, 120_000, f.ago(1_000), phone);
    let earlier = position(episode, 30_000, f.ago(3_000), phone);
    let results = f
        .push(&[first.clone(), later.clone(), earlier.clone()])
        .await;

    assert_result(&results[0], &first, "applied", f.ago(2_000));
    assert_result(&results[1], &later, "applied", f.ago(1_000));
    assert_result(&results[2], &earlier, "superseded", f.ago(3_000));
    let state = f.state(episode).await.expect("the episode has state");
    assert_eq!(state.podcast_id, f.podcast_id);
    assert_eq!(state.position_ms, 120_000);
    assert_eq!(state.position_at, Some(f.ago(1_000)));
    assert_eq!(state.position_dev, Some(phone));
    assert_eq!(state.version, 2, "a superseded mutation writes nothing");
    assert_eq!(f.version_counter().await, 2);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_exact_tie_goes_to_the_greater_device_id(pool: PgPool) {
    let f = fixture(pool).await;
    let (smaller, greater) = devices();
    let at = f.ago(1_000);

    let from_greater = played(f.episodes[0], true, at, greater);
    let from_smaller = played(f.episodes[0], false, at, smaller);
    let results = f.push(&[from_greater.clone(), from_smaller.clone()]).await;
    assert_result(&results[0], &from_greater, "applied", at);
    assert_result(&results[1], &from_smaller, "superseded", at);

    let from_smaller = archived(f.episodes[1], true, at, smaller);
    let from_greater = archived(f.episodes[1], false, at, greater);
    let results = f.push(&[from_smaller.clone(), from_greater.clone()]).await;
    assert_result(&results[0], &from_smaller, "applied", at);
    assert_result(&results[1], &from_greater, "applied", at);

    let first = f.state(f.episodes[0]).await.expect("state exists");
    assert!(first.played);
    assert_eq!(first.played_dev, Some(greater));
    let second = f.state(f.episodes[1]).await.expect("state exists");
    assert!(!second.archived);
    assert_eq!(second.archived_dev, Some(greater));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_exact_tie_from_the_same_device_goes_to_the_later_mutation(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();
    let at = f.ago(1_000);

    let older = position(f.episodes[0], 60_000, at, phone);
    let newer = position(f.episodes[0], 90_000, at, phone);
    let results = f.push(&[older.clone(), newer.clone()]).await;

    assert_result(&results[0], &older, "applied", at);
    assert_result(&results[1], &newer, "applied", at);
    let state = f.state(f.episodes[0]).await.expect("state exists");
    assert_eq!(state.position_ms, 90_000);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_fast_clock_is_clamped_and_the_result_carries_the_stored_at(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();
    let an_hour_ahead = plus_ms(f.app.now(), 3_600_000);
    let clamped = plus_ms(f.app.now(), 60_000);

    let mutation = position(f.episodes[0], 60_000, an_hour_ahead, phone);
    let results = f.push(std::slice::from_ref(&mutation)).await;

    assert_result(&results[0], &mutation, "applied", clamped);
    let state = f.state(f.episodes[0]).await.expect("state exists");
    assert_eq!(state.position_at, Some(clamped));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn each_mutation_writes_only_its_own_field(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, laptop) = devices();
    let episode = f.episodes[0];

    f.push(&[position(episode, 60_000, f.ago(3_000), phone)])
        .await;
    f.push(&[played(episode, true, f.ago(2_000), laptop)]).await;

    let state = f.state(episode).await.expect("state exists");
    assert_eq!(state.position_ms, 60_000);
    assert_eq!(state.position_at, Some(f.ago(3_000)));
    assert_eq!(state.position_dev, Some(phone));
    assert!(state.played);
    assert_eq!(state.played_at, Some(f.ago(2_000)));
    assert_eq!(state.played_dev, Some(laptop));
    assert!(!state.archived);
    assert_eq!(state.archived_at, None);
    assert_eq!(state.archived_dev, None);
    assert_eq!(state.version, 2);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_position_has_no_upper_bound(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();
    let ten_days_ms = 864_000_000;

    let mutation = position(f.episodes[0], ten_days_ms, f.ago(1_000), phone);
    let results = f.push(std::slice::from_ref(&mutation)).await;

    assert_result(&results[0], &mutation, "applied", f.ago(1_000));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn settings_store_unknown_keys_and_any_json_value(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, laptop) = devices();

    let speed = setting("playback_speed", json!(1.3), f.ago(3_000), phone);
    let unknown = setting(
        "future_feature",
        json!({ "a": [1, null] }),
        f.ago(3_000),
        phone,
    );
    let cleared = setting("future_feature", Value::Null, f.ago(2_000), laptop);
    let stale = setting("playback_speed", json!(2.0), f.ago(4_000), laptop);
    let results = f
        .push(&[
            speed.clone(),
            unknown.clone(),
            cleared.clone(),
            stale.clone(),
        ])
        .await;

    assert_result(&results[0], &speed, "applied", f.ago(3_000));
    assert_result(&results[1], &unknown, "applied", f.ago(3_000));
    assert_result(&results[2], &cleared, "applied", f.ago(2_000));
    assert_result(&results[3], &stale, "superseded", f.ago(4_000));
    let rows = sqlx::query!(
        r#"select key, value, value_at as "value_at: SqlTimestamp", value_dev, version
           from user_setting where user_id = $1 order by key"#,
        f.user_id.as_uuid()
    )
    .fetch_all(&f.app.pool)
    .await
    .expect("user_setting is readable");
    let rows: Vec<_> = rows
        .into_iter()
        .map(|row| {
            (
                row.key,
                row.value,
                row.value_at.0,
                row.value_dev,
                row.version,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "future_feature".to_owned(),
                Value::Null,
                f.ago(2_000),
                laptop.as_uuid(),
                3
            ),
            (
                "playback_speed".to_owned(),
                json!(1.3),
                f.ago(3_000),
                phone.as_uuid(),
                1
            ),
        ]
    );
}

// ==============================================================================
// Replays
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_replayed_mutation_id_returns_the_stored_result(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, laptop) = devices();
    let episode = f.episodes[0];
    let id = MutationId::new_v7();
    let an_hour_ahead = plus_ms(f.app.now(), 3_600_000);
    let original = position_with_id(id, episode, 60_000, an_hour_ahead, phone);

    let first = f.push(std::slice::from_ref(&original)).await;
    // The clock moves on and a newer write lands. A fresh evaluation would
    // now clamp differently and be superseded; the replay must not.
    f.app.advance_ms(120_000);
    f.push(&[position(
        episode,
        90_000,
        plus_ms(f.app.now(), 30_000),
        laptop,
    )])
    .await;
    let counter = f.version_counter().await;
    let replayed = f.push(std::slice::from_ref(&original)).await;

    assert_eq!(replayed, first);
    assert_eq!(
        f.version_counter().await,
        counter,
        "a replay writes nothing"
    );
    let state = f.state(episode).await.expect("state exists");
    assert_eq!(state.position_ms, 90_000);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_mutation_id_repeated_within_one_batch_is_applied_once(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();
    let id = MutationId::new_v7();
    let mutation = position_with_id(id, f.episodes[0], 60_000, f.ago(1_000), phone);

    let results = f.push(&[mutation.clone(), mutation.clone()]).await;

    assert_eq!(results[0], results[1]);
    assert_result(&results[0], &mutation, "applied", f.ago(1_000));
    assert_eq!(f.version_counter().await, 1);
}

// ==============================================================================
// Rejections and request errors
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_invalid_mutation_is_rejected_without_failing_the_batch(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();
    let at = f.ago(1_000);
    let before = position(f.episodes[0], 60_000, at, phone);
    let unknown_type =
        json!({ "id": MutationId::new_v7(), "at": at, "dev": phone, "type": "episode.starred" });
    let negative = position(f.episodes[1], -1, at, phone);
    let unknown_episode = played(unknown_episode_id(), true, at, phone);
    let unreadable_at = json!({ "id": MutationId::new_v7(), "at": "yesterday", "dev": phone,
        "type": "episode.played", "episode_id": f.episodes[2], "played": true });
    let after = played(f.episodes[2], true, at, phone);

    let results = f
        .push(&[
            before.clone(),
            unknown_type.clone(),
            negative.clone(),
            unknown_episode.clone(),
            unreadable_at.clone(),
            after.clone(),
        ])
        .await;

    assert_result(&results[0], &before, "applied", at);
    assert_result(&results[1], &unknown_type, "rejected", at);
    assert_eq!(results[1]["code"], "invalid_mutation");
    assert_result(&results[2], &negative, "rejected", at);
    assert_eq!(results[2]["code"], "invalid_mutation");
    assert_result(&results[3], &unknown_episode, "rejected", at);
    assert_eq!(results[3]["code"], "unknown_episode");
    assert_eq!(results[4]["id"], unreadable_at["id"]);
    assert_eq!(results[4]["status"], "rejected");
    assert_eq!(results[4]["code"], "invalid_mutation");
    assert!(results[4].get("at").is_none(), "{}", results[4]);
    assert_result(&results[5], &after, "applied", at);
    assert_eq!(f.state(f.episodes[1]).await, None);
    assert_eq!(f.version_counter().await, 2);

    let replayed = f
        .push(&[unknown_type, negative, unknown_episode, unreadable_at])
        .await;
    assert_eq!(replayed, results[1..5]);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn more_than_200_mutations_is_batch_too_large(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();
    let mutations: Vec<Value> = (0..201)
        .map(|_| position(f.episodes[0], 1_000, f.ago(1_000), phone))
        .collect();

    let response = f.sync(json!({ "mutations": mutations })).await;

    assert_error(&response, StatusCode::PAYLOAD_TOO_LARGE, "batch_too_large");
    assert_eq!(f.state(f.episodes[0]).await, None);

    let response = f.sync(json!({ "mutations": mutations[..200] })).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_broken_envelope_is_bad_request(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();

    let not_json = f.sync_raw("{".to_owned()).await;
    assert_error(&not_json, StatusCode::BAD_REQUEST, "bad_request");
    let not_an_array = f.sync(json!({ "mutations": {} })).await;
    assert_error(&not_an_array, StatusCode::BAD_REQUEST, "bad_request");
    let valid = position(f.episodes[0], 1_000, f.ago(1_000), phone);
    let missing_id = json!({ "at": f.ago(1_000), "dev": phone, "type": "episode.played",
        "episode_id": f.episodes[0], "played": true });
    let response = f.sync(json!({ "mutations": [valid, missing_id] })).await;
    assert_error(&response, StatusCode::BAD_REQUEST, "bad_request");
    assert_eq!(f.state(f.episodes[0]).await, None, "nothing is applied");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn sync_requires_a_session(pool: PgPool) {
    let app = TestApp::new(pool);
    let response = app
        .send(
            Request::post("/v1/sync")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"mutations":[]}"#))
                .expect("valid request"),
        )
        .await;
    assert_error(&response, StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn subscription_sets_apply_in_a_batch_with_other_mutations(pool: PgPool) {
    let f = fixture(pool).await;
    let (phone, _) = devices();
    let before = position(f.episodes[0], 60_000, f.ago(1_000), phone);
    let subscribe = json!({ "id": MutationId::new_v7(), "at": f.ago(1_000), "dev": phone,
        "type": "subscription.set", "podcast_id": f.podcast_id,
        "feed_url": "https://example.com/feed.xml", "subscribed": true });
    // The app's fetch policy has no allowlist, so a LAN address is refused.
    let refused = json!({ "id": MutationId::new_v7(), "at": f.ago(1_000), "dev": phone,
        "type": "subscription.set", "podcast_id": f.podcast_id,
        "feed_url": "http://192.168.1.20/feed.xml", "subscribed": true });

    let results = f
        .push(&[before.clone(), subscribe.clone(), refused.clone()])
        .await;

    assert_result(&results[0], &before, "applied", f.ago(1_000));
    assert_result(&results[1], &subscribe, "applied", f.ago(1_000));
    assert_result(&results[2], &refused, "rejected", f.ago(1_000));
    assert_eq!(results[2]["code"], "invalid_feed_url");
    assert_eq!(f.version_counter().await, 2);
}

// ==============================================================================
// Response shape
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn the_response_has_server_time_and_scopes_only_when_asked(pool: PgPool) {
    let f = fixture(pool).await;

    let push_only = f.sync(json!({ "mutations": [] })).await;
    assert_eq!(push_only.status, StatusCode::OK, "{}", push_only.body);
    assert_eq!(push_only.body["server_time"], json!(f.app.now()));
    assert_eq!(push_only.body["results"], json!([]));
    assert!(push_only.body.get("scopes").is_none(), "{}", push_only.body);

    let with_scopes = f
        .sync(
            json!({ "scopes": { "user": 7, "podcasts": [{ "id": f.podcast_id, "version": 3 }] } }),
        )
        .await;
    assert_eq!(with_scopes.status, StatusCode::OK, "{}", with_scopes.body);
    assert_eq!(with_scopes.body["scopes"]["user"]["version"], 7);
    assert_eq!(
        with_scopes.body["scopes"]["podcasts"][0]["id"],
        json!(f.podcast_id)
    );
    // The fixture user has no subscription to the podcast.
    assert_eq!(
        with_scopes.body["scopes"]["podcasts"][0]["status"],
        "not_subscribed"
    );
}

// ==============================================================================
// Concurrency
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn concurrent_batches_from_one_user_serialize(pool: PgPool) {
    let user_id = common::create_user(&pool, "alice").await;
    let podcast_id = common::create_podcast(&pool, "https://example.com/feed.xml").await;
    let episodes = common::create_episodes(&pool, podcast_id, 2).await;
    let (phone, laptop) = devices();
    let now = Timestamp::now();

    // Holding the user lock makes both batches queue behind it, so they
    // provably contend instead of happening to run one after the other.
    let holder = VersionScope::begin_user(&pool, user_id)
        .await
        .expect("the user row locks")
        .expect("the user exists");
    let tasks: Vec<_> = [(episodes[0], phone), (episodes[1], laptop)]
        .into_iter()
        .map(|(episode_id, dev)| {
            let pool = pool.clone();
            let mutation = Mutation::EpisodePlayed(EpisodePlayed {
                id: MutationId::new_v7(),
                at: now,
                dev,
                episode_id,
                played: true,
            });
            tokio::spawn(async move {
                let policy = FetchPolicy::new(Allowlist::default());
                apply_batch(&pool, user_id, &[Ok(mutation)], now, &policy).await
            })
        })
        .collect();
    common::wait_for_lock_waiters(&pool, 2).await;
    holder.commit().await.expect("the holder commits");

    for task in tasks {
        let results = task
            .await
            .expect("the task doesn't panic")
            .expect("the batch applies");
        assert!(matches!(
            results[0].outcome,
            MutationOutcome::Applied { .. }
        ));
    }
    let mut versions = Vec::new();
    for episode_id in episodes {
        let state = episode_state(&pool, user_id, episode_id)
            .await
            .expect("state exists");
        versions.push(state.version);
    }
    versions.sort_unstable();
    assert_eq!(versions, [1, 2]);
    assert_eq!(version_counter(&pool, user_id).await, 2);
}

// ==============================================================================
// Conformance: `conformance/lww.json` apply cases
// ==============================================================================

#[derive(Debug, Deserialize)]
struct LwwFixture {
    apply: Vec<ApplyCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyCase {
    name: String,
    server_now: Timestamp,
    before: Option<Field>,
    mutations: Vec<Field>,
    results: Vec<Value>,
    after: Field,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Field {
    value: Value,
    at: Timestamp,
    dev: DeviceId,
}

/// The fixture's fields are generic. A number runs as `episode.position`
/// and a boolean as `episode.played`, which covers every case in the file.
fn field_mutation(episode_id: EpisodeId, field: &Field) -> Mutation {
    let (id, at, dev) = (MutationId::new_v7(), field.at, field.dev);
    match &field.value {
        Value::Number(ms) => Mutation::EpisodePosition(EpisodePosition {
            id,
            at,
            dev,
            episode_id,
            position_ms: ms.as_i64().expect("fixture positions are integers"),
        }),
        Value::Bool(played) => Mutation::EpisodePlayed(EpisodePlayed {
            id,
            at,
            dev,
            episode_id,
            played: *played,
        }),
        other => panic!("no mutation type stores {other}"),
    }
}

fn stored_field(state: &EpisodeState, sample: &Value) -> Field {
    let (value, at, dev) = match sample {
        Value::Number(_) => (
            json!(state.position_ms),
            state.position_at,
            state.position_dev,
        ),
        _ => (json!(state.played), state.played_at, state.played_dev),
    };
    Field {
        value,
        at: at.expect("the field was written"),
        dev: dev.expect("the field was written"),
    }
}

/// An episode id no podcast has.
fn unknown_episode_id() -> EpisodeId {
    EpisodeId::from_uuid(MutationId::new_v7().as_uuid())
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn conformance_apply_cases(pool: PgPool) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/lww.json");
    let text = std::fs::read_to_string(&path).expect("conformance/lww.json is checked in");
    let fixture: LwwFixture = serde_json::from_str(&text).expect("the fixture parses");
    let podcast_id = common::create_podcast(&pool, "https://example.com/feed.xml").await;
    let episodes = common::create_episodes(&pool, podcast_id, fixture.apply.len()).await;
    let policy = FetchPolicy::new(Allowlist::default());

    for (index, (case, episode_id)) in fixture.apply.iter().zip(episodes).enumerate() {
        let user_id = common::create_user(&pool, &format!("user{index}")).await;
        if let Some(before) = &case.before {
            let seeded = apply_batch(
                &pool,
                user_id,
                &[Ok(field_mutation(episode_id, before))],
                before.at,
                &policy,
            )
            .await
            .expect("the seed applies");
            assert!(
                matches!(seeded[0].outcome, MutationOutcome::Applied { at } if at == before.at),
                "{}: seeding stores `before` unclamped",
                case.name
            );
        }
        let mutations: Vec<_> = case
            .mutations
            .iter()
            .map(|field| Ok(field_mutation(episode_id, field)))
            .collect();

        let results = apply_batch(&pool, user_id, &mutations, case.server_now, &policy)
            .await
            .expect("the batch applies");

        let results: Vec<Value> = results
            .iter()
            .map(|result| {
                let mut value = serde_json::to_value(result).expect("results serialize");
                value
                    .as_object_mut()
                    .expect("a result is an object")
                    .remove("id");
                value
            })
            .collect();
        assert_eq!(results, case.results, "{}", case.name);
        let state = episode_state(&pool, user_id, episode_id)
            .await
            .expect("the field was written");
        assert_eq!(
            stored_field(&state, &case.after.value),
            case.after,
            "{}",
            case.name
        );
    }
}
