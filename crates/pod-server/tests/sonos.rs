//! Playback on Sonos (spec 13) against a stub speaker.
//!
//! Each test runs its own stub on a port from [`bind_test_port`]
//! (`POD_TEST_SONOS_PORT` pins it, which then needs `--test-threads=1`). The
//! stub answers the UPnP actions pod-server sends like a lone S2 speaker
//! would, keeps the transport state, URI, position and volume those actions
//! set, and records every action so tests can check what was sent. Tests
//! move the speaker on (the episode ends, the Sonos app switches source) by
//! editing that state, and run tracker passes with [`Sonos::tick`].

mod auth_app;
mod common;
mod read_seed;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use auth_app::{PASSWORD, TestApp, assert_error, token_of};
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use pod_protocol::{DeviceId, EpisodeId, PodcastId, Timestamp, UserId};
use pod_server::db::SqlTimestamp;
use pod_server::sonos::Sonos;
use pod_server::sonos::upnp::{format_rel_time, leaves, parse_rel_time, soap_action};
use quick_xml::escape::escape;
use read_seed::{insert_episode, insert_podcast, set_subscription};
use serde_json::{Value, json};
use sqlx::PgPool;
use url::Url;

use common::port::bind_test_port;

const PLAYER: &str = "RINCON_000E5800000201400";

// ==============================================================================
// Stub speaker
// ==============================================================================

/// What the stub speaker plays, as its UPnP actions report it.
#[derive(Debug)]
struct SpeakerState {
    transport: &'static str,
    uri: String,
    position_ms: i64,
    volume: u8,
    /// Every action received, with its arguments, in order.
    actions: Vec<(String, HashMap<String, String>)>,
    /// Answer every request with HTTP 503, like a speaker mid-reboot.
    down: bool,
}

struct Stub {
    state: Arc<Mutex<SpeakerState>>,
    base: Url,
}

impl Stub {
    async fn start() -> Stub {
        let listener = bind_test_port("POD_TEST_SONOS_PORT");
        let address = listener
            .local_addr()
            .expect("a bound listener has an address");
        let base = Url::parse(&format!("http://{address}/")).expect("valid URL");
        let state = Arc::new(Mutex::new(SpeakerState {
            transport: "STOPPED",
            uri: "x-rincon-queue:RINCON_000E5800000201400#0".into(),
            position_ms: 0,
            volume: 10,
            actions: Vec::new(),
            down: false,
        }));
        let router = axum::Router::new()
            .route("/{*path}", post(answer))
            .with_state((Arc::clone(&state), base.clone()));
        let listener =
            tokio::net::TcpListener::from_std(listener).expect("the listener joins the runtime");
        tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("the stub serves until the test ends");
        });
        Stub { state, base }
    }

    fn with<T>(&self, change: impl FnOnce(&mut SpeakerState) -> T) -> T {
        change(&mut self.state.lock().expect("no test panics while holding it"))
    }

    /// The actions that change the speaker, in order. Queries are left out.
    fn commands(&self) -> Vec<String> {
        self.with(|state| {
            state
                .actions
                .iter()
                .map(|(action, _)| action.clone())
                .filter(|action| !action.starts_with("Get"))
                .collect()
        })
    }

    /// The arguments of the latest `action` received.
    fn arguments(&self, action: &str) -> HashMap<String, String> {
        self.with(|state| {
            state
                .actions
                .iter()
                .rev()
                .find(|(name, _)| name == action)
                .map(|(_, arguments)| arguments.clone())
                .unwrap_or_else(|| panic!("the speaker received {action}"))
        })
    }

    /// The service for this stub. Starting sessions get no start task, so
    /// [`Household::tick`] decides when their seek goes out.
    fn sonos(&self) -> Arc<Sonos> {
        Arc::new(
            Sonos::new(vec![self.base.clone()])
                .expect("the client builds")
                .expect("a seed turns Sonos on")
                .without_start_tasks(),
        )
    }

    /// Waits for the tracker's detached seek to reach the stub.
    async fn wait_for_position(&self, position_ms: i64) {
        for _ in 0..200 {
            if self.with(|state| state.position_ms) == position_ms {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the stub never got the seek to {position_ms}");
    }
}

async fn answer(
    axum::extract::State((state, base)): axum::extract::State<(Arc<Mutex<SpeakerState>>, Url)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let action = headers
        .get("soapaction")
        .and_then(|value| value.to_str().ok())
        .and_then(soap_action)
        .expect("every request names its action")
        .to_owned();
    let arguments = leaves(&body).expect("requests are XML");
    let mut state = state.lock().expect("no test panics while holding it");
    state.actions.push((action.clone(), arguments.clone()));
    if state.down {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let fields: Vec<(&str, String)> = match action.as_str() {
        "GetZoneGroupState" => vec![("ZoneGroupState", zone_group_state(&base))],
        "SetAVTransportURI" => {
            state.uri = arguments["CurrentURI"].clone();
            state.transport = "STOPPED";
            state.position_ms = 0;
            vec![]
        }
        "Seek" => {
            assert_eq!(arguments["Unit"], "REL_TIME");
            state.position_ms = parse_rel_time(&arguments["Target"]).expect("H:MM:SS targets");
            vec![]
        }
        "Play" => {
            if arguments["Speed"] != "1" {
                return fault(717);
            }
            state.transport = "PLAYING";
            vec![]
        }
        "Pause" => {
            state.transport = "PAUSED_PLAYBACK";
            vec![]
        }
        "GetTransportInfo" => vec![
            ("CurrentTransportState", state.transport.into()),
            ("CurrentTransportStatus", "OK".into()),
            ("CurrentSpeed", "1".into()),
        ],
        "GetPositionInfo" => vec![
            ("Track", "1".into()),
            ("TrackDuration", "0:00:00".into()),
            ("TrackURI", state.uri.clone()),
            ("RelTime", format_rel_time(state.position_ms)),
        ],
        "GetMediaInfo" => vec![
            ("NrTracks", "1".into()),
            ("CurrentURI", state.uri.clone()),
            ("CurrentURIMetaData", String::new()),
        ],
        "GetGroupVolume" => vec![("CurrentVolume", state.volume.to_string())],
        "SetGroupVolume" => {
            state.volume = arguments["DesiredVolume"].parse().expect("a volume");
            vec![]
        }
        other => panic!("unexpected action {other}"),
    };
    let fields: String = fields
        .iter()
        .map(|(name, value)| format!("<{name}>{}</{name}>", escape(value.as_str())))
        .collect();
    let body = format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action}Response xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">{fields}</u:{action}Response></s:Body></s:Envelope>"#
    );
    ([(header::CONTENT_TYPE, "text/xml")], body).into_response()
}

fn fault(code: u32) -> Response {
    let body = format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>{code}</errorCode></UPnPError></detail></s:Fault></s:Body></s:Envelope>"#
    );
    (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
}

/// One group, "Couch", whose coordinator is the stub itself.
fn zone_group_state(base: &Url) -> String {
    format!(
        r#"<ZoneGroupState><ZoneGroups><ZoneGroup Coordinator="{PLAYER}" ID="{PLAYER}:1"><ZoneGroupMember UUID="{PLAYER}" Location="{base}xml/device_description.xml" ZoneName="Couch"/></ZoneGroup></ZoneGroups><VanishedDevices></VanishedDevices></ZoneGroupState>"#
    )
}

// ==============================================================================
// Setup
// ==============================================================================

struct Household {
    app: TestApp,
    stub: Stub,
    sonos: Arc<Sonos>,
    token: String,
    user_id: UserId,
    episode_id: EpisodeId,
}

const SHOW: &str = "Fish & Chips <Live>";

fn podcast_id(n: u8) -> PodcastId {
    format!("5a0f3c1e-2b4d-4e6f-8a1b-3c5d7e9f00{n:02x}")
        .parse()
        .expect("valid UUID")
}

/// A signed-in user subscribed to a podcast with one episode, and a stub
/// speaker the server knows.
async fn household(pool: PgPool) -> Household {
    let stub = Stub::start().await;
    let sonos = stub.sonos();
    let app = TestApp::with_sonos(pool.clone(), Arc::clone(&sonos));
    let (token, user_id) = register(&app, "alice").await;
    let podcast_id = podcast_id(1);
    insert_podcast(&pool, podcast_id, SHOW).await;
    let episode_id = EpisodeId::new(podcast_id, "episode-1");
    insert_episode(&pool, podcast_id, episode_id, Timestamp::now(), false).await;
    set_subscription(&pool, user_id, podcast_id, true, Timestamp::now()).await;
    Household {
        app,
        stub,
        sonos,
        token,
        user_id,
        episode_id,
    }
}

async fn register(app: &TestApp, username: &str) -> (String, UserId) {
    let response = app.post_auth("/v1/auth/register", username, PASSWORD).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let user_id = serde_json::from_value(response.body["user_id"].clone())
        .expect("register returns the user id");
    (token_of(&response), user_id)
}

impl Household {
    async fn post(&self, path: &str, body: Option<Value>) -> auth_app::TestResponse {
        self.post_as(&self.token, path, body).await
    }

    async fn post_as(
        &self,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> auth_app::TestResponse {
        let request = Request::post(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json");
        let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
        self.app
            .send(request.body(body).expect("valid request"))
            .await
    }

    async fn play(&self, position_ms: i64) -> auth_app::TestResponse {
        self.post(
            &format!("/v1/sonos/players/{PLAYER}/play"),
            Some(json!({ "episode_id": self.episode_id, "position_ms": position_ms })),
        )
        .await
    }

    async fn command(&self, command: Value) -> auth_app::TestResponse {
        self.post(
            &format!("/v1/sonos/players/{PLAYER}/command"),
            Some(command),
        )
        .await
    }

    async fn players(&self, token: &str) -> auth_app::TestResponse {
        self.app
            .with_token(Method::GET, "/v1/sonos/players", token)
            .await
    }

    /// Plays the episode from `position_ms` and takes it through the start:
    /// one pass sees the group play from 0:00 and seeks, the next sees the
    /// seek landed and writes the position.
    async fn started(&self, position_ms: i64) {
        let response = self.play(position_ms).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        self.tick().await;
        self.stub.wait_for_position(position_ms).await;
        self.tick().await;
    }

    async fn tick(&self) {
        self.sonos
            .tick(&self.app.pool, self.app.now())
            .await
            .expect("the tracker pass succeeds");
    }

    async fn session_row(&self) -> Option<SessionRow> {
        sqlx::query!(
            "select device_id, position_ms, playing, played_written, seek_to_ms
             from sonos_session where player_id = $1",
            PLAYER
        )
        .fetch_optional(&self.app.pool)
        .await
        .expect("the session table is readable")
        .map(|row| SessionRow {
            device_id: DeviceId::from_uuid(row.device_id),
            position_ms: row.position_ms,
            playing: row.playing,
            played_written: row.played_written,
            seek_to_ms: row.seek_to_ms,
        })
    }

    /// `(position_ms, position_at, position_dev, played)` of the episode's
    /// state, or `None` if nothing was written.
    async fn episode_state(&self) -> Option<(i64, Option<Timestamp>, Option<DeviceId>, bool)> {
        sqlx::query!(
            r#"select position_ms, position_at as "position_at: SqlTimestamp", position_dev,
                      played
               from episode_state where user_id = $1 and episode_id = $2"#,
            self.user_id.as_uuid(),
            self.episode_id.as_uuid()
        )
        .fetch_optional(&self.app.pool)
        .await
        .expect("the state table is readable")
        .map(|row| {
            (
                row.position_ms,
                row.position_at.map(|at| at.0),
                row.position_dev.map(DeviceId::from_uuid),
                row.played,
            )
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SessionRow {
    device_id: DeviceId,
    position_ms: i64,
    playing: bool,
    played_written: bool,
    seek_to_ms: Option<i64>,
}

// ==============================================================================
// Tests
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn play_starts_from_the_top_and_seeks_once_the_group_plays(pool: PgPool) {
    let home = household(pool).await;

    let response = home.play(683_000).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let pod_player = json!({
        "id": PLAYER,
        "name": "Couch",
        "volume": 10,
        "state": "pod",
        "session": { "episode_id": home.episode_id, "playing": true, "position_ms": 683_000 },
    });
    assert_eq!(
        response.body, pod_player,
        "clients see the start position while the group plays from 0:00"
    );

    // No seek before Play: a slow host's stream makes the speaker give up.
    assert_eq!(home.stub.commands(), ["SetAVTransportURI", "Play"]);
    let audio_url = format!("https://media.example.com/{}.mp3", home.episode_id);
    let load = home.stub.arguments("SetAVTransportURI");
    assert_eq!(load["CurrentURI"], audio_url);
    let metadata = leaves(&load["CurrentURIMetaData"]).expect("the metadata is XML");
    assert_eq!(metadata["title"], format!("Episode {}", home.episode_id));
    assert_eq!(metadata["creator"], SHOW);
    assert_eq!(metadata["res"], audio_url);
    assert_eq!(
        metadata["albumArtURI"],
        format!("https://images.example.com/{}.jpg", podcast_id(1))
    );
    assert_eq!(home.stub.arguments("Play")["Speed"], "1");

    let row = home.session_row().await.expect("play records the session");
    assert_eq!(
        (
            row.position_ms,
            row.playing,
            row.played_written,
            row.seek_to_ms
        ),
        (683_000, false, false, Some(683_000))
    );

    // The group plays from 0:00: the pass seeks and writes nothing.
    home.tick().await;
    home.stub.wait_for_position(683_000).await;
    assert_eq!(home.stub.arguments("Seek")["Target"], "0:11:23");
    assert_eq!(home.episode_state().await, None, "0:00 is never written");

    // The next pass sees the seek landed.
    home.tick().await;
    let row = home.session_row().await.expect("the session goes on");
    assert_eq!((row.playing, row.seek_to_ms), (true, None));
    assert_eq!(
        home.episode_state().await.map(|state| state.0),
        Some(683_000)
    );

    let listed = home.players(&home.token).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body, json!({ "players": [pod_player] }));

    // Another user sees the group as taken, without the session.
    let (bob, _) = register(&home.app, "bob").await;
    let listed = home.players(&bob).await;
    assert_eq!(
        listed.body["players"][0],
        json!({ "id": PLAYER, "name": "Couch", "volume": 10, "state": "other", "session": null })
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_stream_lost_mid_episode_stays_and_resume_restarts_it_where_it_was(pool: PgPool) {
    let home = household(pool).await;
    sqlx::query!(
        "update episode set duration_ms = 3600000 where id = $1",
        home.episode_id.as_uuid()
    )
    .execute(&home.app.pool)
    .await
    .expect("the episode gets a duration");
    home.started(683_000).await;
    home.stub.with(|state| state.position_ms = 700_000);
    home.tick().await;

    // The speaker lost the stream: it stops and reports 0:00:00, an hour
    // before the end.
    home.stub.with(|state| {
        state.transport = "STOPPED";
        state.position_ms = 0;
    });
    home.tick().await;
    let row = home.session_row().await.expect("the session stays");
    assert_eq!((row.position_ms, row.playing), (700_000, false));
    assert_eq!(
        home.episode_state().await.map(|state| (state.0, state.3)),
        Some((700_000, false)),
        "not played, and 0 isn't written"
    );

    // Play would start the stream at 0:00, so resume loads it again and
    // seeks back once it plays.
    let resumed = home.command(json!({ "type": "resume" })).await;
    assert_eq!(resumed.status, StatusCode::OK, "{}", resumed.body);
    assert_eq!(
        resumed.body["session"],
        json!({ "episode_id": home.episode_id, "playing": true, "position_ms": 700_000 })
    );
    assert_eq!(
        home.stub.commands(),
        [
            "SetAVTransportURI",
            "Play",
            "Seek",
            "SetAVTransportURI",
            "Play"
        ]
    );
    home.tick().await;
    home.stub.wait_for_position(700_000).await;
    home.tick().await;
    let row = home.session_row().await.expect("the session goes on");
    assert_eq!((row.playing, row.seek_to_ms), (true, None));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_idle_group_lists_as_idle_and_one_playing_something_else_as_other(pool: PgPool) {
    let home = household(pool).await;
    let idle =
        json!({ "id": PLAYER, "name": "Couch", "volume": 10, "state": "idle", "session": null });
    assert_eq!(
        home.players(&home.token).await.body,
        json!({ "players": [idle] })
    );

    home.stub.with(|state| state.transport = "PLAYING");
    assert_eq!(
        home.players(&home.token).await.body["players"][0]["state"],
        "other"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_tick_while_playing_writes_the_position_as_the_session(pool: PgPool) {
    let home = household(pool).await;
    home.started(683_000).await;
    let session = home.session_row().await.expect("play records the session");

    home.app.advance_ms(5_000);
    home.stub.with(|state| state.position_ms = 700_000);
    home.tick().await;

    assert_eq!(
        home.episode_state().await,
        Some((
            700_000,
            Some(home.app.now()),
            Some(session.device_id),
            false
        ))
    );
    assert_eq!(
        home.session_row().await,
        Some(SessionRow {
            position_ms: 700_000,
            ..session
        })
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn stopped_after_playing_marks_played_and_ends_without_writing_zero(pool: PgPool) {
    let home = household(pool).await;
    home.started(683_000).await;
    home.stub.with(|state| state.position_ms = 1_800_000);
    home.tick().await;

    // The stream ran out: the speaker stops and reports 0:00:00. The feed
    // gives no duration, so a stop after playing is the end.
    home.stub.with(|state| {
        state.transport = "STOPPED";
        state.position_ms = 0;
    });
    home.app.advance_ms(5_000);
    home.tick().await;

    let (position_ms, _, _, played) = home.episode_state().await.expect("the state was written");
    assert_eq!((position_ms, played), (1_800_000, true));
    assert_eq!(home.session_row().await, None, "the session ended");
    assert_eq!(
        home.players(&home.token).await.body["players"][0]["state"],
        "idle"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_stream_that_never_plays_is_not_marked_played(pool: PgPool) {
    let home = household(pool).await;
    assert_eq!(home.play(683_000).await.status, StatusCode::OK);
    // The speaker can't fetch the stream and gives up before a poll sees
    // it play.
    home.stub.with(|state| {
        state.transport = "STOPPED";
        state.position_ms = 0;
    });
    home.tick().await;

    assert_eq!(home.episode_state().await, None, "nothing is written");
    assert!(
        home.session_row().await.is_some(),
        "the session stays until the user releases it"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_command_after_the_group_changed_source_ends_the_session(pool: PgPool) {
    let home = household(pool).await;
    assert_eq!(home.play(683_000).await.status, StatusCode::OK);
    // The Sonos app started the queue since the last poll.
    home.stub.with(|state| {
        state.uri = "x-rincon-queue:RINCON_000E5800000201400#0".into();
        state.transport = "PLAYING";
    });

    let paused = home.command(json!({ "type": "pause" })).await;
    assert_error(&paused, StatusCode::CONFLICT, "no_session");
    assert_eq!(
        home.stub.commands(),
        ["SetAVTransportURI", "Play"],
        "the queue keeps playing"
    );
    assert_eq!(home.session_row().await, None);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn pausing_a_paused_group_sends_no_pause(pool: PgPool) {
    let home = household(pool).await;
    home.started(10_000).await;
    home.stub.with(|state| state.transport = "PAUSED_PLAYBACK");

    let paused = home.command(json!({ "type": "pause" })).await;
    assert_eq!(paused.status, StatusCode::OK, "{}", paused.body);
    assert_eq!(
        paused.body["session"],
        json!({ "episode_id": home.episode_id, "playing": false, "position_ms": 10_000 })
    );
    assert_eq!(home.stub.commands(), ["SetAVTransportURI", "Play", "Seek"]);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_seek_while_starting_moves_where_the_session_starts(pool: PgPool) {
    let home = household(pool).await;
    assert_eq!(home.play(683_000).await.status, StatusCode::OK);

    let skipped = home
        .command(json!({ "type": "skip", "delta_ms": 30_000 }))
        .await;
    assert_eq!(skipped.status, StatusCode::OK, "{}", skipped.body);
    assert_eq!(skipped.body["session"]["position_ms"], 713_000);
    assert_eq!(
        home.stub.commands(),
        ["SetAVTransportURI", "Play"],
        "the group still plays from 0:00, so nothing is sent yet"
    );

    home.tick().await;
    home.stub.wait_for_position(713_000).await;
    assert_eq!(home.stub.arguments("Seek")["Target"], "0:11:53");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_new_source_on_the_group_ends_the_session(pool: PgPool) {
    let home = household(pool).await;
    home.started(683_000).await;
    let written = home.episode_state().await;

    // The Sonos app started the queue.
    home.stub.with(|state| {
        state.uri = "x-rincon-queue:RINCON_000E5800000201400#0".into();
        state.position_ms = 12_000;
    });
    home.app.advance_ms(5_000);
    home.tick().await;

    assert_eq!(home.session_row().await, None);
    assert_eq!(
        home.episode_state().await,
        written,
        "nothing more is written"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn release_pauses_writes_the_position_and_ends_the_session(pool: PgPool) {
    let home = household(pool).await;
    home.started(683_000).await;
    let session = home.session_row().await.expect("play records the session");
    home.stub.with(|state| state.position_ms = 720_000);
    home.app.advance_ms(5_000);

    let response = home
        .post(&format!("/v1/sonos/players/{PLAYER}/release"), None)
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(
        response.body,
        json!({ "episode_id": home.episode_id, "position_ms": 720_000, "playing": true })
    );
    assert_eq!(
        home.stub.commands(),
        ["SetAVTransportURI", "Play", "Seek", "Pause"]
    );
    assert_eq!(home.session_row().await, None);
    assert_eq!(
        home.episode_state().await,
        Some((
            720_000,
            Some(home.app.now()),
            Some(session.device_id),
            false
        ))
    );

    let again = home
        .post(&format!("/v1/sonos/players/{PLAYER}/release"), None)
        .await;
    assert_error(&again, StatusCode::CONFLICT, "no_session");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn commands_drive_the_speaker_and_write_the_position(pool: PgPool) {
    let home = household(pool).await;
    home.started(10_000).await;
    let device_id = home.session_row().await.expect("a session").device_id;

    home.app.advance_ms(1_000);
    let paused = home.command(json!({ "type": "pause" })).await;
    assert_eq!(paused.status, StatusCode::OK, "{}", paused.body);
    assert_eq!(
        paused.body["session"],
        json!({ "episode_id": home.episode_id, "playing": false, "position_ms": 10_000 })
    );
    assert_eq!(
        home.episode_state().await,
        Some((10_000, Some(home.app.now()), Some(device_id), false))
    );

    home.app.advance_ms(1_000);
    let skipped = home
        .command(json!({ "type": "skip", "delta_ms": -15_000 }))
        .await;
    assert_eq!(skipped.status, StatusCode::OK, "{}", skipped.body);
    assert_eq!(
        skipped.body["session"]["position_ms"], 0,
        "skips clamp at 0"
    );
    assert_eq!(home.stub.arguments("Seek")["Target"], "0:00:00");

    home.app.advance_ms(1_000);
    let resumed = home.command(json!({ "type": "resume" })).await;
    assert_eq!(resumed.body["session"]["playing"], true, "{}", resumed.body);
    let row = home.session_row().await.expect("a session");
    assert_eq!((row.position_ms, row.playing), (0, true));

    let louder = home
        .command(json!({ "type": "volume", "volume": 35 }))
        .await;
    assert_eq!(louder.body["volume"], 35, "{}", louder.body);

    let too_loud = home
        .command(json!({ "type": "volume", "volume": 101 }))
        .await;
    assert_error(&too_loud, StatusCode::BAD_REQUEST, "bad_request");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn commands_need_the_users_own_session_except_volume(pool: PgPool) {
    let home = household(pool).await;
    let pause = home.command(json!({ "type": "pause" })).await;
    assert_error(&pause, StatusCode::CONFLICT, "no_session");

    // Someone else's session doesn't count either.
    assert_eq!(home.play(0).await.status, StatusCode::OK);
    let (bob, _) = register(&home.app, "bob").await;
    let seek = home
        .post_as(
            &bob,
            &format!("/v1/sonos/players/{PLAYER}/command"),
            Some(json!({ "type": "seek", "position_ms": 5_000 })),
        )
        .await;
    assert_error(&seek, StatusCode::CONFLICT, "no_session");

    let volume = home
        .post_as(
            &bob,
            &format!("/v1/sonos/players/{PLAYER}/command"),
            Some(json!({ "type": "volume", "volume": 20 })),
        )
        .await;
    assert_eq!(volume.status, StatusCode::OK, "{}", volume.body);
    assert_eq!(volume.body["volume"], 20);
    assert_eq!(volume.body["state"], "other");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn unknown_players_and_unreadable_episodes_are_not_found(pool: PgPool) {
    let home = household(pool.clone()).await;
    let unknown = home
        .post(
            "/v1/sonos/players/RINCON_000E5800000101400/play",
            Some(json!({ "episode_id": home.episode_id, "position_ms": 0 })),
        )
        .await;
    assert_error(&unknown, StatusCode::NOT_FOUND, "not_found");
    let unknown = home
        .post(
            "/v1/sonos/players/RINCON_000E5800000101400/command",
            Some(json!({ "type": "volume", "volume": 20 })),
        )
        .await;
    assert_error(&unknown, StatusCode::NOT_FOUND, "not_found");

    // An episode of a podcast the user isn't subscribed to.
    let other_podcast = podcast_id(2);
    insert_podcast(&pool, other_podcast, "Elsewhere").await;
    let foreign = EpisodeId::new(other_podcast, "episode-1");
    insert_episode(&pool, other_podcast, foreign, Timestamp::now(), false).await;
    // And a deleted one of the user's podcast.
    let deleted = EpisodeId::new(podcast_id(1), "episode-gone");
    insert_episode(&pool, podcast_id(1), deleted, Timestamp::now(), true).await;
    for episode_id in [foreign, deleted] {
        let response = home
            .post(
                &format!("/v1/sonos/players/{PLAYER}/play"),
                Some(json!({ "episode_id": episode_id, "position_ms": 0 })),
            )
            .await;
        assert_error(&response, StatusCode::NOT_FOUND, "not_found");
    }
    assert!(
        home.stub.commands().is_empty(),
        "nothing reached the speaker"
    );

    let negative = home.play(-1).await;
    assert_error(&negative, StatusCode::BAD_REQUEST, "bad_request");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_failing_speaker_is_upstream_unavailable(pool: PgPool) {
    let home = household(pool).await;
    home.stub.with(|state| state.down = true);
    let listed = home.players(&home.token).await;
    assert_error(&listed, StatusCode::BAD_GATEWAY, "upstream_unavailable");
    let play = home.play(0).await;
    assert_error(&play, StatusCode::BAD_GATEWAY, "upstream_unavailable");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn an_unreachable_group_ends_its_session_after_two_minutes(pool: PgPool) {
    let home = household(pool).await;
    assert_eq!(home.play(683_000).await.status, StatusCode::OK);
    home.stub.with(|state| state.down = true);

    home.tick().await;
    home.app.advance_ms(119_000);
    home.tick().await;
    assert!(home.session_row().await.is_some(), "the session waits");

    home.app.advance_ms(1_000);
    home.tick().await;
    assert_eq!(home.session_row().await, None);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn without_sonos_hosts_the_list_is_empty_and_players_are_not_found(pool: PgPool) {
    let app = TestApp::new(pool);
    let (token, _) = register(&app, "alice").await;
    let listed = app
        .with_token(Method::GET, "/v1/sonos/players", &token)
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body, json!({ "players": [] }));

    let release = app
        .with_token(
            Method::POST,
            &format!("/v1/sonos/players/{PLAYER}/release"),
            &token,
        )
        .await;
    assert_error(&release, StatusCode::NOT_FOUND, "not_found");
}
