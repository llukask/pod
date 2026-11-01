//! Connect between client devices (spec 14): the event stream, reports,
//! commands and transfers. Sonos groups as Connect devices are tested in
//! `sonos.rs`, beside the stub speaker.
//!
//! The test app gives Connect a 300 ms grace period and a 500 ms release
//! wait, so the timeouts don't slow these down.

mod auth_app;
mod common;
mod connect_events;
mod read_seed;

use std::time::Duration;

use auth_app::{PASSWORD, TestApp, TestResponse, assert_error, token_of};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use connect_events::Events;
use pod_protocol::{DeviceId, EpisodeId, PodcastId, Timestamp, UserId};
use read_seed::{insert_episode, insert_podcast, set_episode_state, set_subscription};
use serde_json::{Value, json};
use sqlx::PgPool;

/// One user signed in on two devices, subscribed to a podcast with two
/// episodes.
struct Account {
    app: TestApp,
    user_id: UserId,
    /// The phone's token and device id.
    phone: String,
    phone_id: DeviceId,
    /// The laptop's.
    laptop: String,
    laptop_id: DeviceId,
    podcast_id: PodcastId,
    episode: EpisodeId,
    other_episode: EpisodeId,
}

async fn account(pool: PgPool) -> Account {
    let app = TestApp::new(pool.clone());
    let registered = app.post_auth("/v1/auth/register", "alice", PASSWORD).await;
    assert_eq!(registered.status, StatusCode::OK, "{}", registered.body);
    let logged_in = app.post_auth("/v1/auth/login", "alice", PASSWORD).await;
    assert_eq!(logged_in.status, StatusCode::OK, "{}", logged_in.body);
    let device = |response: &TestResponse| -> DeviceId {
        serde_json::from_value(response.body["device_id"].clone()).expect("a device id")
    };
    let user_id = serde_json::from_value(registered.body["user_id"].clone()).expect("a user id");
    let podcast_id: PodcastId = "5a0f3c1e-2b4d-4e6f-8a1b-3c5d7e9f0001"
        .parse()
        .expect("test values are valid");
    insert_podcast(&pool, podcast_id, "The Show").await;
    let episode = EpisodeId::new(podcast_id, "episode-1");
    let other_episode = EpisodeId::new(podcast_id, "episode-2");
    for id in [episode, other_episode] {
        insert_episode(&pool, podcast_id, id, Timestamp::now(), false).await;
    }
    set_subscription(&pool, user_id, podcast_id, true, Timestamp::now()).await;
    Account {
        user_id,
        phone: token_of(&registered),
        phone_id: device(&registered),
        laptop: token_of(&logged_in),
        laptop_id: device(&logged_in),
        app,
        podcast_id,
        episode,
        other_episode,
    }
}

impl Account {
    async fn post(&self, token: &str, path: &str, body: Value) -> TestResponse {
        self.app
            .send(
                Request::post(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("valid request"),
            )
            .await
    }

    async fn report(
        &self,
        token: &str,
        episode: EpisodeId,
        position_ms: i64,
        playing: bool,
    ) -> TestResponse {
        self.post(
            token,
            "/v1/connect/report",
            json!({ "episode_id": episode, "position_ms": position_ms,
                    "playing": playing, "speed": 1.0 }),
        )
        .await
    }

    async fn command(&self, token: &str, command: Value) -> TestResponse {
        self.post(token, "/v1/connect/command", command).await
    }

    async fn transfer(&self, token: &str, device_id: &str) -> TestResponse {
        self.post(
            token,
            "/v1/connect/transfer",
            json!({ "device_id": device_id }),
        )
        .await
    }

    async fn state(&self, token: &str) -> Value {
        let response = self.app.with_token(Method::GET, "/v1/connect", token).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        response.body
    }
}

fn device_ids(state: &Value) -> Vec<String> {
    state["devices"]
        .as_array()
        .expect("devices is a list")
        .iter()
        .map(|device| device["id"].as_str().expect("an id").to_owned())
        .collect()
}

// ==============================================================================
// Event stream
// ==============================================================================

#[sqlx::test(migrations = "./migrations")]
async fn a_stream_starts_with_the_state_and_lists_every_connected_device(pool: PgPool) {
    let account = account(pool).await;
    let mut phone = Events::open(&account.app, &account.phone, "android").await;
    let first = phone.next().await;
    assert_eq!(first["type"], "state");
    assert_eq!(first["playback"], Value::Null);
    assert_eq!(
        first["devices"],
        json!([{ "id": account.phone_id, "name": "Pixel 8", "kind": "android" }])
    );

    let mut laptop = Events::open(&account.app, &account.laptop, "web").await;
    let both = vec![account.phone_id.to_string(), account.laptop_id.to_string()];
    assert_eq!(device_ids(&laptop.next().await), both);
    assert_eq!(device_ids(&phone.next_of("state").await), both);
    assert_eq!(device_ids(&account.state(&account.phone).await), both);

    drop(laptop);
    let alone = phone
        .state_where(|state| device_ids(state).len() == 1)
        .await;
    assert_eq!(device_ids(&alone), vec![account.phone_id.to_string()]);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_stream_needs_a_known_kind_and_a_token(pool: PgPool) {
    let account = account(pool).await;
    for path in ["/v1/connect/events", "/v1/connect/events?kind=toaster"] {
        let response = account
            .app
            .with_token(Method::GET, path, &account.phone)
            .await;
        assert_error(&response, StatusCode::BAD_REQUEST, "bad_request");
    }
    let response = account
        .app
        .send(
            Request::get("/v1/connect/events?kind=web")
                .body(Body::empty())
                .expect("test values are valid"),
        )
        .await;
    assert_error(&response, StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[sqlx::test(migrations = "./migrations")]
async fn a_second_stream_of_a_device_replaces_the_first(pool: PgPool) {
    let account = account(pool).await;
    let mut first_tab = Events::open(&account.app, &account.laptop, "web").await;
    first_tab.next_of("state").await;
    let mut second_tab = Events::open(&account.app, &account.laptop, "web").await;
    assert_eq!(first_tab.next_of("replaced").await["type"], "replaced");
    assert!(first_tab.ends_within(Duration::from_secs(2)).await);
    let state = second_tab.next_of("state").await;
    assert_eq!(device_ids(&state), vec![account.laptop_id.to_string()]);
}

// ==============================================================================
// Reports
// ==============================================================================

#[sqlx::test(migrations = "./migrations")]
async fn playing_claims_the_playback_and_releases_the_previous_device(pool: PgPool) {
    let account = account(pool).await;
    let mut phone = Events::open(&account.app, &account.phone, "android").await;
    let mut laptop = Events::open(&account.app, &account.laptop, "desktop").await;

    let response = account
        .report(&account.phone, account.episode, 60_000, true)
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.body["device_id"], json!(account.phone_id));
    assert_eq!(
        response.body["episode"]["title"],
        format!("Episode {}", account.episode)
    );
    assert_eq!(response.body["episode"]["podcast_title"], "The Show");
    let seen = laptop
        .state_where(|state| state["playback"]["device_id"] == json!(account.phone_id))
        .await;
    assert_eq!(seen["playback"]["playing"], true);

    // The laptop starts the other episode: it takes over and the phone
    // pauses.
    let response = account
        .report(&account.laptop, account.other_episode, 0, true)
        .await;
    assert_eq!(response.body["device_id"], json!(account.laptop_id));
    assert_eq!(
        phone.next_of("release").await,
        json!({ "type": "release", "transfer_id": null })
    );
    let seen = phone
        .state_where(|state| state["playback"]["device_id"] == json!(account.laptop_id))
        .await;
    assert_eq!(
        seen["playback"]["episode"]["id"],
        json!(account.other_episode)
    );

    // The phone's pause after its release changes nothing, and its answer
    // tells it that it is a remote now.
    let response = account
        .report(&account.phone, account.episode, 61_000, false)
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["device_id"], json!(account.laptop_id));
    assert_eq!(response.body["episode"]["id"], json!(account.other_episode));
}

#[sqlx::test(migrations = "./migrations")]
async fn a_paused_report_claims_only_when_nothing_is_active(pool: PgPool) {
    let account = account(pool).await;
    let _phone = Events::open(&account.app, &account.phone, "android").await;
    let _laptop = Events::open(&account.app, &account.laptop, "web").await;

    let response = account
        .report(&account.phone, account.episode, 5_000, false)
        .await;
    assert_eq!(response.body["device_id"], json!(account.phone_id));
    assert_eq!(response.body["playing"], false);

    let response = account
        .report(&account.laptop, account.other_episode, 0, false)
        .await;
    assert_eq!(response.body["device_id"], json!(account.phone_id));
    assert_eq!(response.body["position_ms"], 5_000);
}

#[sqlx::test(migrations = "./migrations")]
async fn reports_need_a_stream_and_a_readable_episode(pool: PgPool) {
    let account = account(pool).await;
    let response = account
        .report(&account.phone, account.episode, 0, true)
        .await;
    assert_error(&response, StatusCode::CONFLICT, "no_session");

    let _phone = Events::open(&account.app, &account.phone, "android").await;
    let unknown = EpisodeId::new(account.podcast_id, "never-ingested");
    let response = account.report(&account.phone, unknown, 0, true).await;
    assert_error(&response, StatusCode::NOT_FOUND, "not_found");
    let response = account
        .post(
            &account.phone,
            "/v1/connect/report",
            json!({ "episode_id": account.episode, "position_ms": 0,
                    "playing": true, "speed": 0 }),
        )
        .await;
    assert_error(&response, StatusCode::BAD_REQUEST, "bad_request");
}

#[sqlx::test(migrations = "./migrations")]
async fn the_position_advances_with_time_and_speed_while_playing(pool: PgPool) {
    let account = account(pool).await;
    let _phone = Events::open(&account.app, &account.phone, "android").await;
    let response = account
        .post(
            &account.phone,
            "/v1/connect/report",
            json!({ "episode_id": account.episode, "position_ms": 10_000,
                    "playing": true, "speed": 2.0 }),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let position = account.state(&account.laptop).await["playback"]["position_ms"]
        .as_i64()
        .expect("a position");
    assert!(
        (10_400..12_000).contains(&position),
        "position {position} should have advanced by twice 200 ms"
    );
}

// ==============================================================================
// Device gone
// ==============================================================================

#[sqlx::test(migrations = "./migrations")]
async fn an_active_device_that_stays_away_leaves_the_playback_paused(pool: PgPool) {
    let account = account(pool).await;
    let phone = Events::open(&account.app, &account.phone, "android").await;
    let mut laptop = Events::open(&account.app, &account.laptop, "web").await;
    account
        .report(&account.phone, account.episode, 30_000, true)
        .await;
    drop(phone);

    // Gone from the list at once, but still active during the grace period.
    let state = laptop
        .state_where(|state| device_ids(state).len() == 1)
        .await;
    assert_eq!(state["playback"]["device_id"], json!(account.phone_id));
    let state = laptop
        .state_where(|state| state["playback"]["device_id"].is_null())
        .await;
    assert_eq!(state["playback"]["playing"], false);
    assert_eq!(state["playback"]["position_ms"], 30_000);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_device_back_within_the_grace_period_stays_active(pool: PgPool) {
    let account = account(pool).await;
    let phone = Events::open(&account.app, &account.phone, "android").await;
    account
        .report(&account.phone, account.episode, 30_000, true)
        .await;
    drop(phone);
    let mut phone = Events::open(&account.app, &account.phone, "android").await;
    // Well past the 300 ms grace.
    let last = phone
        .quiet_for(Duration::from_millis(700))
        .await
        .expect("the reconnect brings a state");
    assert_eq!(last["playback"]["device_id"], json!(account.phone_id));
}

// ==============================================================================
// Commands
// ==============================================================================

#[sqlx::test(migrations = "./migrations")]
async fn commands_go_to_the_active_device(pool: PgPool) {
    let account = account(pool).await;
    let mut phone = Events::open(&account.app, &account.phone, "android").await;
    let _laptop = Events::open(&account.app, &account.laptop, "web").await;

    let response = account
        .command(&account.laptop, json!({ "type": "pause" }))
        .await;
    assert_error(&response, StatusCode::CONFLICT, "no_session");

    account
        .report(&account.phone, account.episode, 0, true)
        .await;
    for command in [
        json!({ "type": "pause" }),
        json!({ "type": "skip", "delta_ms": -15_000 }),
        json!({ "type": "play", "episode_id": account.other_episode }),
    ] {
        let response = account.command(&account.laptop, command.clone()).await;
        assert_eq!(response.status, StatusCode::ACCEPTED, "{}", response.body);
        assert_eq!(response.body["device_id"], json!(account.phone_id));
        assert_eq!(
            phone.next_of("command").await,
            json!({ "type": "command", "command": command })
        );
    }

    let response = account
        .command(&account.laptop, json!({ "type": "volume", "volume": 20 }))
        .await;
    assert_error(&response, StatusCode::BAD_REQUEST, "bad_request");
    let unknown = EpisodeId::new(account.podcast_id, "never-ingested");
    let response = account
        .command(
            &account.laptop,
            json!({ "type": "play", "episode_id": unknown }),
        )
        .await;
    assert_error(&response, StatusCode::NOT_FOUND, "not_found");
}

// ==============================================================================
// Transfer
// ==============================================================================

#[sqlx::test(migrations = "./migrations")]
async fn a_transfer_releases_the_source_and_loads_the_target(pool: PgPool) {
    let account = account(pool).await;
    let mut phone = Events::open(&account.app, &account.phone, "android").await;
    let mut laptop = Events::open(&account.app, &account.laptop, "desktop").await;
    account
        .report(&account.phone, account.episode, 100_000, true)
        .await;

    let transfer = {
        let laptop_id = account.laptop_id.to_string();
        let account = &account;
        async move { account.transfer(&account.laptop, &laptop_id).await }
    };
    let source = async {
        let release = phone.next_of("release").await;
        let transfer_id = release["transfer_id"].clone();
        assert!(transfer_id.is_string(), "{release}");
        let response = account
            .post(
                &account.phone,
                "/v1/connect/report",
                json!({ "episode_id": account.episode, "position_ms": 101_500,
                        "playing": false, "speed": 1.0, "released": transfer_id }),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    };
    let (response, ()) = tokio::join!(transfer, source);
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.body["device_id"], json!(account.laptop_id));
    assert_eq!(response.body["position_ms"], 101_500);
    assert_eq!(response.body["playing"], false);
    assert_eq!(
        laptop.next_of("load").await,
        json!({ "type": "load", "episode_id": account.episode, "position_ms": 101_500 })
    );

    // The target's own report then shows it playing.
    let response = account
        .report(&account.laptop, account.episode, 101_600, true)
        .await;
    assert_eq!(response.body["device_id"], json!(account.laptop_id));
    let state = phone
        .state_where(|state| state["playback"]["playing"] == true)
        .await;
    assert_eq!(state["playback"]["device_id"], json!(account.laptop_id));
}

#[sqlx::test(migrations = "./migrations")]
async fn a_silent_source_leaves_the_transfer_its_last_position(pool: PgPool) {
    let account = account(pool).await;
    let mut phone = Events::open(&account.app, &account.phone, "android").await;
    let mut laptop = Events::open(&account.app, &account.laptop, "web").await;
    account
        .report(&account.phone, account.episode, 100_000, false)
        .await;

    let response = account
        .transfer(&account.phone, &account.laptop_id.to_string())
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    phone.next_of("release").await;
    assert_eq!(
        laptop.next_of("load").await,
        json!({ "type": "load", "episode_id": account.episode, "position_ms": 100_000 })
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn a_transfer_from_nothing_leaves_the_start_position_to_the_target(pool: PgPool) {
    let account = account(pool.clone()).await;
    set_episode_state(
        &pool,
        account.user_id,
        account.podcast_id,
        account.episode,
        42_000,
        false,
        false,
    )
    .await;
    let mut laptop = Events::open(&account.app, &account.laptop, "web").await;
    // Rebuilt from the newest position write, as after a restart.
    let state = laptop.next_of("state").await;
    assert_eq!(state["playback"]["device_id"], Value::Null);
    assert_eq!(state["playback"]["episode"]["id"], json!(account.episode));
    assert_eq!(state["playback"]["position_ms"], 42_000);

    let response = account
        .transfer(&account.laptop, &account.laptop_id.to_string())
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(
        laptop.next_of("load").await,
        json!({ "type": "load", "episode_id": account.episode, "position_ms": null })
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn transfers_need_a_playback_and_a_known_target(pool: PgPool) {
    let account = account(pool).await;
    let _phone = Events::open(&account.app, &account.phone, "android").await;
    let response = account
        .transfer(&account.phone, &account.phone_id.to_string())
        .await;
    assert_error(&response, StatusCode::CONFLICT, "no_session");

    account
        .report(&account.phone, account.episode, 0, true)
        .await;
    // The laptop is signed in but has no stream, and Sonos is off.
    for target in [
        account.laptop_id.to_string(),
        "RINCON_000E5800000201400".into(),
    ] {
        let response = account.transfer(&account.phone, &target).await;
        assert_error(&response, StatusCode::NOT_FOUND, "not_found");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn transferring_to_the_paused_active_device_resumes_it(pool: PgPool) {
    let account = account(pool).await;
    let mut phone = Events::open(&account.app, &account.phone, "android").await;
    account
        .report(&account.phone, account.episode, 0, false)
        .await;
    let response = account
        .transfer(&account.laptop, &account.phone_id.to_string())
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(
        phone.next_of("command").await,
        json!({ "type": "command", "command": { "type": "resume" } })
    );
}
