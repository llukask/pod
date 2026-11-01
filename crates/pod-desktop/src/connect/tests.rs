//! Connect in the real window (spec 14), against a stub pod-server that
//! keeps the account's playback the way pod-server does: it holds this
//! device's event stream, claims for reports, relays commands, runs
//! transfers, plays the Sonos groups' part, and records every request.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse as _, Response};
use color_eyre::eyre::{Result, WrapErr as _};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use pod_protocol::{
    ConnectCommand, ConnectDevice, ConnectEvent, ConnectReport, ConnectState, DeviceKind,
    EpisodeId, ErrorBody, ErrorCode, Playback, PlaybackEpisode, SonosPlayer, SonosPlayerState,
    SonosPlayersResponse, TransferRequest,
};
use rusqlite::{Transaction, params};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

use crate::app::tests::{TestApp, signed_in};
use crate::player::mpv::{Command, Event};
use crate::player::{LAST_EPISODE_ID, PlayerHandle};
use crate::test_port::bind_test_port;

const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
const EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c01";
/// An episode this device has no row for.
const UNKNOWN: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c99";
/// The device id [`signed_in`] seeds.
const OWN: &str = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d";
const PHONE: &str = "019a3f10-8c2e-7a41-9b3d-000000000001";
const COUCH: &str = "RINCON_804AF246458401400";
const KITCHEN: &str = "RINCON_B8E937000000001400";
const DURATION_MS: i64 = 3_120_000;
const HOUR_MS: i64 = 60 * 60 * 1000;
const STREAM: &str = "https://example.com/pager.mp3";

// ==============================================================================
// The stub server
// ==============================================================================

/// The account as the stub's server sees it, and what it heard.
#[derive(Debug)]
struct Account {
    devices: Vec<ConnectDevice>,
    playback: Option<Playback>,
    /// This device's open event streams.
    streams: Vec<mpsc::UnboundedSender<ConnectEvent>>,
    players: Vec<SonosPlayer>,
    /// `(path, body)` of every `POST` but syncs, oldest first.
    requests: Vec<(String, Value)>,
    /// Answers the next command or transfer with this status and code.
    fail_command: Option<(StatusCode, ErrorCode)>,
    fail_transfer: Option<(StatusCode, ErrorCode)>,
}

impl Account {
    /// Sends `event` to every stream of this device.
    fn send(&mut self, event: &ConnectEvent) {
        self.streams
            .retain(|stream| stream.send(event.clone()).is_ok());
    }

    fn broadcast(&mut self) {
        let state = ConnectEvent::State(ConnectState {
            devices: self.devices.clone(),
            playback: self.playback.clone(),
        });
        self.send(&state);
    }

    fn active(&self) -> Option<&str> {
        self.playback.as_ref()?.device_id.as_deref()
    }
}

struct Stub {
    account: Arc<Mutex<Account>>,
    _stop: oneshot::Sender<()>,
}

impl Stub {
    /// Serves on a port from [`bind_test_port`] (`POD_TEST_CONNECT_UI_PORT`
    /// overrides the choice) and points the seeded account at the stub.
    /// "Pixel 8" is connected besides this device, the account has
    /// `playback`, and the household has "Couch", free, and "Kitchen",
    /// playing something else.
    fn start(app: &TestApp, playback: Option<Playback>) -> Stub {
        let listener = bind_test_port("POD_TEST_CONNECT_UI_PORT");
        let base_url = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("a bound listener has an address")
        );
        let group = |id: &str, name: &str, state| SonosPlayer {
            id: id.to_owned(),
            name: name.to_owned(),
            volume: 12,
            state,
            session: None,
        };
        let account = Arc::new(Mutex::new(Account {
            devices: vec![
                device(OWN, "Pod desktop", DeviceKind::Desktop),
                device(PHONE, "Pixel 8", DeviceKind::Android),
            ],
            playback,
            streams: Vec::new(),
            players: vec![
                group(COUCH, "Couch", SonosPlayerState::Idle),
                group(KITCHEN, "Kitchen", SonosPlayerState::Other),
            ],
            requests: Vec::new(),
            fail_command: None,
            fail_transfer: None,
        }));
        // The `released` of the last report, for a transfer waiting on it.
        let (released, _) = watch::channel(None::<Uuid>);

        let events = {
            let account = account.clone();
            move |request: axum::extract::Request| {
                let account = account.clone();
                async move {
                    assert_authorized(&request);
                    assert_eq!(request.uri().query(), Some("kind=desktop"));
                    let (sender, receiver) = mpsc::unbounded_channel();
                    {
                        let mut account = account.lock().expect("not poisoned");
                        account.streams.push(sender);
                        account.broadcast();
                    }
                    let events = futures_util::stream::unfold(receiver, |mut receiver| async {
                        let event = receiver.recv().await?;
                        Some((
                            Ok::<_, Infallible>(SseEvent::default().data(event.to_json())),
                            receiver,
                        ))
                    });
                    Sse::new(events)
                }
            }
        };
        let fallback = {
            let account = account.clone();
            move |request: axum::extract::Request| {
                let account = account.clone();
                let released = released.clone();
                async move {
                    assert_authorized(&request);
                    let path = request.uri().path().to_owned();
                    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                        .await
                        .expect("the body arrives");
                    answer(&account, &released, &path, &body).await
                }
            }
        };
        let router = axum::Router::new()
            .route("/v1/connect/events", axum::routing::get(events))
            .fallback(fallback);
        app.runtime
            .block_on(app.store.write(move |transaction| {
                transaction
                    .execute("update account set server_url = ?1", [base_url])
                    .context("point the account at the stub")
            }))
            .expect("the account update is written");
        let listener = {
            let _runtime = app.runtime.enter();
            tokio::net::TcpListener::from_std(listener).expect("the listener joins the runtime")
        };
        let (stop, stopped) = oneshot::channel::<()>();
        app.background.spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("the stub runs");
        });
        Stub {
            account,
            _stop: stop,
        }
    }

    fn with<T>(&self, change: impl FnOnce(&mut Account) -> T) -> T {
        change(&mut self.account.lock().expect("not poisoned"))
    }

    /// Replaces the playback and tells the device, as a change on another
    /// device would.
    fn set_playback(&self, playback: Option<Playback>) {
        self.with(|account| {
            account.playback = playback;
            account.broadcast();
        });
    }

    fn send(&self, event: ConnectEvent) {
        self.with(|account| account.send(&event));
    }

    fn streams(&self) -> usize {
        self.with(|account| {
            account.streams.retain(|stream| !stream.is_closed());
            account.streams.len()
        })
    }

    /// The bodies of the requests to `path`, oldest first.
    fn bodies(&self, path: &str) -> Vec<Value> {
        self.with(|account| {
            account
                .requests
                .iter()
                .filter(|(to, _)| to == path)
                .map(|(_, body)| body.clone())
                .collect()
        })
    }

    fn reports(&self) -> Vec<Value> {
        self.bodies("/v1/connect/report")
    }

    fn commands(&self) -> Vec<Value> {
        self.bodies("/v1/connect/command")
    }
}

impl Drop for Stub {
    /// Open streams would keep the graceful shutdown waiting.
    fn drop(&mut self) {
        self.with(|account| account.streams.clear());
    }
}

fn assert_authorized(request: &axum::extract::Request) {
    assert!(
        request
            .headers()
            .get("authorization")
            .is_some_and(|value| value == "Bearer secret"),
        "{} carries the token",
        request.uri()
    );
}

/// Plays pod-server's part for one request (spec 14.3 to 14.5).
async fn answer(
    account: &Mutex<Account>,
    released: &watch::Sender<Option<Uuid>>,
    path: &str,
    body: &[u8],
) -> Response {
    let error = |status: StatusCode, code: ErrorCode| {
        (status, axum::Json(ErrorBody::new(code, "the stub says no"))).into_response()
    };
    let lock = || account.lock().expect("not poisoned");
    if path == "/v1/sync" {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if path == "/v1/sonos/players" {
        return axum::Json(SonosPlayersResponse {
            players: lock().players.clone(),
        })
        .into_response();
    }
    let value: Value = serde_json::from_slice(body).expect("a JSON body");
    lock().requests.push((path.to_owned(), value));
    match path {
        "/v1/connect/report" => {
            let report: ConnectReport = serde_json::from_slice(body).expect("a report");
            let mut account = lock();
            let claims = report.playing || matches!(account.active(), None | Some(OWN));
            if claims {
                account.playback = Some(Playback {
                    device_id: Some(OWN.to_owned()),
                    episode: episode_of(report.episode_id),
                    position_ms: report.position_ms,
                    playing: report.playing,
                    speed: report.speed,
                    volume: None,
                });
                account.broadcast();
            }
            if report.released.is_some() {
                released.send_replace(report.released);
            }
            match &account.playback {
                Some(playback) => axum::Json(playback.clone()).into_response(),
                None => error(StatusCode::CONFLICT, ErrorCode::NoSession),
            }
        }
        "/v1/connect/command" => {
            let command: ConnectCommand = serde_json::from_slice(body).expect("a command");
            let mut account = lock();
            if let Some((status, code)) = account.fail_command.take() {
                return error(status, code);
            }
            let Some(playback) = account.playback.as_mut() else {
                return error(StatusCode::CONFLICT, ErrorCode::NoSession);
            };
            match playback.device_id.clone().as_deref() {
                None => error(StatusCode::CONFLICT, ErrorCode::NoSession),
                Some(group) if group.starts_with("RINCON_") => {
                    match command {
                        ConnectCommand::Pause => playback.playing = false,
                        ConnectCommand::Resume => playback.playing = true,
                        ConnectCommand::Seek { position_ms } => playback.position_ms = position_ms,
                        ConnectCommand::Skip { delta_ms } => {
                            playback.position_ms = (playback.position_ms + delta_ms).max(0);
                        }
                        ConnectCommand::Volume { volume } => playback.volume = Some(volume),
                        ConnectCommand::Play { episode_id } => {
                            playback.episode = episode_of(episode_id);
                        }
                    }
                    let answer = playback.clone();
                    account.broadcast();
                    axum::Json(answer).into_response()
                }
                Some(device) => {
                    let answer = playback.clone();
                    if device == OWN {
                        account.send(&ConnectEvent::Command { command });
                    }
                    (StatusCode::ACCEPTED, axum::Json(answer)).into_response()
                }
            }
        }
        "/v1/connect/transfer" => {
            let TransferRequest { device_id } =
                serde_json::from_slice(body).expect("a transfer request");
            let source = {
                let mut account = lock();
                if let Some((status, code)) = account.fail_transfer.take() {
                    return error(status, code);
                }
                if account.playback.is_none() {
                    return error(StatusCode::CONFLICT, ErrorCode::NoSession);
                }
                account.active().map(str::to_owned)
            };
            // Release this device first, and wait for its report.
            if source.as_deref() == Some(OWN) {
                let transfer_id = Uuid::now_v7();
                lock().send(&ConnectEvent::Release {
                    transfer_id: Some(transfer_id),
                });
                let mut reports = released.subscribe();
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    reports.wait_for(|released| *released == Some(transfer_id)),
                )
                .await;
            }
            let mut account = lock();
            let Some(mut playback) = account.playback.clone() else {
                return error(StatusCode::CONFLICT, ErrorCode::NoSession);
            };
            playback.device_id = Some(device_id.clone());
            let sonos = device_id.starts_with("RINCON_");
            playback.playing = sonos;
            playback.volume = sonos.then_some(12);
            if device_id == OWN {
                let position_ms = source.is_some().then_some(playback.position_ms);
                account.send(&ConnectEvent::Load {
                    episode_id: playback.episode.id,
                    position_ms,
                });
            }
            account.playback = Some(playback.clone());
            account.broadcast();
            axum::Json(playback).into_response()
        }
        other => panic!("the stub has no {other}"),
    }
}

fn device(id: &str, name: &str, kind: DeviceKind) -> ConnectDevice {
    ConnectDevice {
        id: id.parse().expect("the id is valid"),
        name: name.to_owned(),
        kind,
    }
}

fn episode_of(id: EpisodeId) -> PlaybackEpisode {
    let known = id.to_string() == EPISODE;
    PlaybackEpisode {
        id,
        podcast_id: PODCAST.parse().expect("the id is valid"),
        title: if known {
            "EP 151: The pager".to_owned()
        } else {
            "EP 152: Elsewhere".to_owned()
        },
        podcast_title: Some("Darknet Diaries".to_owned()),
        artwork_url: None,
        duration_ms: Some(DURATION_MS),
    }
}

/// The account's playback of `episode` on `device`, or with nothing active.
fn playback(device: Option<&str>, episode: &str, position_ms: i64, playing: bool) -> Playback {
    Playback {
        device_id: device.map(str::to_owned),
        episode: episode_of(episode.parse().expect("the id is valid")),
        position_ms,
        playing,
        speed: 1.0,
        volume: device
            .is_some_and(|device| device.starts_with("RINCON_"))
            .then_some(12),
    }
}

// ==============================================================================
// Helpers
// ==============================================================================

/// Signed in, with one episode heard to 12:34 (or played to the end) and
/// remembered as the last one played, so the bar shows it at start.
fn seed(played: bool) -> impl FnOnce(&Transaction<'_>) -> Result<()> + Send + 'static {
    move |transaction| {
        let now = pod_protocol::Timestamp::now().as_epoch_ms();
        signed_in(transaction)?;
        transaction
            .execute_batch(&format!(
                "insert into podcast (id, feed_url, status, title, version)
                     values ('{PODCAST}', 'https://example.com/feed', 'ok', 'Darknet Diaries', 1);
                 insert into subscription
                     (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
                     values ('{PODCAST}', 'https://example.com/feed', 1, {subscribed}, 'dev');
                 insert into episode (id, podcast_id, title, summary_html, content_html,
                         published_at, duration_ms, audio_url, audio_type, version)
                     values ('{EPISODE}', '{PODCAST}', 'EP 151: The pager', '', '', {published},
                             {DURATION_MS}, '{STREAM}', 'audio/mpeg', 1);",
                subscribed = now - 240 * HOUR_MS,
                published = now - HOUR_MS,
            ))
            .context("insert the podcast and episode")?;
        transaction
            .execute(
                "insert into episode_state (episode_id, podcast_id, position_ms, position_at,
                         position_dev, played, played_at, played_dev)
                     values (?1, ?2, 754000, 1, 'dev', ?3, 1, 'dev')",
                params![EPISODE, PODCAST, played],
            )
            .context("insert the episode state")?;
        transaction
            .execute(
                "insert into device_setting (key, value) values (?1, ?2)",
                params![LAST_EPISODE_ID, format!("\"{EPISODE}\"")],
            )
            .context("remember the episode")?;
        Ok(())
    }
}

fn label(app: &TestApp, cx: &mut TestAppContext, id: &str) -> Option<String> {
    cx.update_window(app.window, |_, window, cx| {
        window.render_frame(cx);
        window
            .try_find(id.to_owned())
            .and_then(|element| element.label().map(str::to_owned))
    })
    .expect("the window is still open")
}

fn shows(app: &TestApp, cx: &mut TestAppContext, id: &str) -> bool {
    cx.update_window(app.window, |_, window, cx| {
        window.render_frame(cx);
        window.try_find(id.to_owned()).is_some()
    })
    .expect("the window is still open")
}

fn click(app: &TestApp, cx: &mut TestAppContext, id: &str) {
    cx.update_window(app.window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id.to_owned(), cx);
    })
    .expect("the window is still open");
}

/// Waits for `done`, rendering frames meanwhile, so answers that arrive on
/// the tokio side reach the window.
fn wait_for(app: &TestApp, cx: &mut TestAppContext, what: &str, done: impl Fn() -> bool) {
    app.wait_until(cx, what, |_| done());
}

/// The next command the player sent mpv.
fn next_command(app: &mut TestApp, cx: &mut TestAppContext) -> Command {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if let Ok(command) = app.mpv_commands.try_recv() {
            return command;
        }
        assert!(Instant::now() < deadline, "timed out waiting for a command");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The queued mutations' payloads, oldest first.
fn outbox(app: &TestApp) -> Vec<Value> {
    app.read(|connection| {
        let mut statement = connection
            .prepare("select payload from outbox order by created_at, mutation_id")
            .context("prepare the outbox query")?;
        let payloads = statement
            .query_map([], |row| row.get::<_, String>(0))
            .context("query the outbox")?
            .map(|payload| {
                serde_json::from_str(&payload.context("read a payload")?).context("parse a payload")
            })
            .collect::<Result<_>>()?;
        Ok(payloads)
    })
}

/// The positions the outbox holds for the episode, oldest first.
fn positions(app: &TestApp) -> Vec<Value> {
    outbox(app)
        .into_iter()
        .filter(|mutation| mutation["type"] == "episode.position")
        .map(|mutation| mutation["position_ms"].clone())
        .collect()
}

/// Opens the window over [`seed`] with the stub holding `playback`, and
/// waits until the stream delivered its first `state`.
fn open(cx: &mut TestAppContext, played: bool, playback: Option<Playback>) -> (TestApp, Stub) {
    let app = TestApp::open(cx, seed(played));
    let stub = Stub::start(&app, playback);
    wait_for(&app, cx, "the stream opens", || stub.streams() > 0);
    app.wait_until(cx, "the player hears the state", |_| true);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cx.update(|cx| cx.global::<PlayerHandle>().0.read(cx).connected()) {
        assert!(Instant::now() < deadline, "timed out waiting for the state");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(2));
    }
    (app, stub)
}

/// [`open`] with nothing played yet, then plays the remembered episode here
/// and lets mpv say it reached 12:40.
fn playing_here(cx: &mut TestAppContext) -> (TestApp, Stub) {
    let (mut app, stub) = open(cx, false, None);
    app.wait_for_label(cx, "player-title", "EP 151: The pager");
    click(&app, cx, "player-toggle");
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: STREAM.to_owned(),
            start_ms: 754_000,
            paused: false,
        }
    );
    for event in [
        Event::Started,
        Event::Duration(DURATION_MS),
        Event::Position(760_000),
    ] {
        app.mpv_events.send(event).expect("the player listens");
    }
    app.wait_for_label(cx, "player-position", "12:40");
    wait_for(&app, cx, "the play is reported", || {
        stub.reports()
            .last()
            .is_some_and(|report| report["playing"] == true)
    });
    (app, stub)
}

// ==============================================================================
// Tests: this device is the active one
// ==============================================================================

#[gpui_kit::test]
fn playing_here_claims_the_playback_with_a_report(cx: &mut TestAppContext) {
    let (_app, stub) = playing_here(cx);
    assert_eq!(
        stub.reports(),
        [json!({
            "episode_id": EPISODE, "position_ms": 754_000, "playing": true,
            "speed": 1.0, "released": null
        })]
    );
    assert_eq!(
        stub.with(|account| account.active().map(str::to_owned)),
        Some(OWN.to_owned())
    );
}

#[gpui_kit::test]
fn a_command_event_runs_as_if_pressed_here(cx: &mut TestAppContext) {
    let (mut app, stub) = playing_here(cx);
    stub.send(ConnectEvent::Command {
        command: ConnectCommand::Pause,
    });
    assert_eq!(next_command(&mut app, cx), Command::Pause(true));
    app.wait_for_label(cx, "player-toggle", "Play");
    wait_for(&app, cx, "the pause is reported", || {
        stub.reports()
            .last()
            .is_some_and(|report| report["playing"] == false)
    });
    assert_eq!(
        stub.reports().last().expect("a report")["position_ms"],
        760_000
    );
    wait_for(&app, cx, "the pause writes", || {
        positions(&app) == [json!(760_000)]
    });

    stub.send(ConnectEvent::Command {
        command: ConnectCommand::Skip { delta_ms: -15_000 },
    });
    assert_eq!(next_command(&mut app, cx), Command::Seek(745_000));
    app.wait_for_label(cx, "player-position", "12:25");
    wait_for(&app, cx, "the skip writes", || {
        positions(&app) == [json!(745_000)]
    });
    wait_for(&app, cx, "the skip is reported", || {
        stub.reports()
            .last()
            .is_some_and(|report| report["position_ms"] == 745_000)
    });
}

#[gpui_kit::test]
fn a_load_starts_at_its_position_and_reports(cx: &mut TestAppContext) {
    let (mut app, stub) = open(cx, true, None);
    stub.send(ConnectEvent::Load {
        episode_id: EPISODE.parse().expect("the id is valid"),
        position_ms: Some(100_000),
    });
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: STREAM.to_owned(),
            start_ms: 100_000,
            paused: false,
        },
        "where the other device left it, though it was played here"
    );
    wait_for(&app, cx, "the start is reported", || {
        !stub.reports().is_empty()
    });
    assert_eq!(
        stub.reports()[0],
        json!({
            "episode_id": EPISODE, "position_ms": 100_000, "playing": true,
            "speed": 1.0, "released": null
        })
    );
    assert!(
        outbox(&app)
            .iter()
            .all(|mutation| mutation["type"] != "episode.played"),
        "not a replay"
    );
}

#[gpui_kit::test]
fn a_load_without_a_position_follows_the_start_rule(cx: &mut TestAppContext) {
    let (mut app, _stub) = open(cx, true, None);
    _stub.send(ConnectEvent::Load {
        episode_id: EPISODE.parse().expect("the id is valid"),
        position_ms: None,
    });
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: STREAM.to_owned(),
            start_ms: 0,
            paused: false,
        },
        "a played episode starts over"
    );
    wait_for(&app, cx, "the replay is written", || {
        outbox(&app)
            .iter()
            .any(|mutation| mutation["type"] == "episode.played" && mutation["played"] == false)
    });
}

#[gpui_kit::test]
fn a_release_pauses_and_reports_released(cx: &mut TestAppContext) {
    let (mut app, stub) = playing_here(cx);
    let transfer_id = Uuid::now_v7();
    stub.send(ConnectEvent::Release {
        transfer_id: Some(transfer_id),
    });
    assert_eq!(next_command(&mut app, cx), Command::Pause(true));
    wait_for(&app, cx, "the release is reported", || {
        stub.reports().len() == 2
    });
    assert_eq!(
        stub.reports()[1],
        json!({
            "episode_id": EPISODE, "position_ms": 760_000, "playing": false,
            "speed": 1.0, "released": transfer_id
        })
    );
    wait_for(&app, cx, "an ordinary pause write", || {
        positions(&app) == [json!(760_000)]
    });

    // The phone took over: this device follows it as a remote.
    stub.set_playback(Some(playback(Some(PHONE), EPISODE, 760_000, true)));
    assert_eq!(next_command(&mut app, cx), Command::Stop);
    app.wait_for_label(cx, "player-where", "Playing on Pixel 8");
    assert_eq!(positions(&app), [json!(760_000)], "stopping writes nothing");
}

// ==============================================================================
// Tests: another device is active
// ==============================================================================

#[gpui_kit::test]
fn the_buttons_steer_the_active_device_and_write_nothing_here(cx: &mut TestAppContext) {
    let (mut app, stub) = open(
        cx,
        false,
        Some(playback(Some(PHONE), EPISODE, 60_000, true)),
    );
    app.wait_for_label(cx, "player-where", "Playing on Pixel 8");
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Pause"));
    assert!(
        shows(&app, cx, "player-speed"),
        "a phone plays at any speed"
    );
    assert!(
        !shows(&app, cx, "player-volume"),
        "only a group has a volume"
    );
    let outbox_before = outbox(&app).len();

    // One at a time, so they reach the stub in order.
    for (button, sent) in [
        ("player-toggle", 1),
        ("player-skip-forward", 2),
        ("player-skip-back", 3),
    ] {
        click(&app, cx, button);
        wait_for(&app, cx, &format!("{button}'s command arrives"), || {
            stub.commands().len() == sent
        });
    }
    assert_eq!(
        stub.commands(),
        [
            json!({ "type": "pause" }),
            json!({ "type": "skip", "delta_ms": 30_000 }),
            json!({ "type": "skip", "delta_ms": -15_000 }),
        ]
    );
    // The phone answers 202 and runs them; until its report, the bar shows
    // what they will do.
    app.wait_for_label(cx, "player-toggle", "Play");
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("1:15"));
    assert!(stub.reports().is_empty(), "a remote doesn't report");
    assert_eq!(
        outbox(&app).len(),
        outbox_before,
        "the active device writes, not this one"
    );
    assert!(app.mpv_commands.try_recv().is_err(), "mpv stays stopped");

    // Playing another episode from a list plays it there.
    cx.update(|cx| crate::player::play(UNKNOWN.parse().expect("the id is valid"), cx));
    wait_for(&app, cx, "the play command arrives", || {
        stub.commands().len() == 4
    });
    assert_eq!(
        stub.commands()[3],
        json!({ "type": "play", "episode_id": UNKNOWN })
    );
}

#[gpui_kit::test]
fn a_sonos_group_shows_its_volume_instead_of_the_speed(cx: &mut TestAppContext) {
    let (app, stub) = open(
        cx,
        false,
        Some(playback(Some(COUCH), EPISODE, 60_000, true)),
    );
    app.wait_for_label(cx, "player-where", "Playing on Couch");
    assert_eq!(
        label(&app, cx, "player-volume").as_deref(),
        Some("Volume on Couch")
    );
    assert!(!shows(&app, cx, "player-speed"), "Sonos plays at 1x only");

    click(&app, cx, "player-volume");
    wait_for(&app, cx, "the volume command arrives", || {
        !stub.commands().is_empty()
    });
    assert_eq!(stub.commands(), [json!({ "type": "volume", "volume": 50 })]);
    app.wait_until(cx, "the slider shows the new volume", |window| {
        window
            .try_find("player-volume")
            .is_some_and(|slider| slider.value() == Some("50"))
    });

    click(&app, cx, "player-toggle");
    app.wait_for_label(cx, "player-toggle", "Play");
    wait_for(&app, cx, "the group pauses", || {
        stub.with(|account| account.playback.as_ref().is_some_and(|p| !p.playing))
    });
}

#[gpui_kit::test]
fn a_playback_this_device_lacks_shows_from_its_metadata(cx: &mut TestAppContext) {
    let (app, _stub) = open(
        cx,
        false,
        Some(playback(Some(PHONE), UNKNOWN, 90_000, false)),
    );
    app.wait_for_label(cx, "player-title", "EP 152: Elsewhere");
    app.wait_for_label(cx, "player-where", "Playing on Pixel 8");
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("1:30"));
}

#[gpui_kit::test]
fn a_failed_command_shows_as_it_was_and_says_so(cx: &mut TestAppContext) {
    let (app, stub) = open(
        cx,
        false,
        Some(playback(Some(COUCH), EPISODE, 60_000, false)),
    );
    app.wait_for_label(cx, "player-where", "Playing on Couch");
    stub.with(|account| {
        account.fail_command = Some((StatusCode::BAD_GATEWAY, ErrorCode::UpstreamUnavailable));
    });
    click(&app, cx, "player-toggle");
    app.wait_for_label(cx, "connect-notice", "Couldn't reach Couch.");
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Play"));
}

#[gpui_kit::test]
fn the_remote_ending_requests_a_sync(cx: &mut TestAppContext) {
    let (app, stub) = open(
        cx,
        false,
        Some(playback(Some(PHONE), EPISODE, 60_000, true)),
    );
    app.wait_for_label(cx, "player-where", "Playing on Pixel 8");
    let _ = app.sync.take_wanted();

    stub.set_playback(Some(playback(None, EPISODE, 95_000, false)));
    app.wait_until(cx, "remote mode ends", |window| {
        window.try_find("player-where").is_none()
    });
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Play"));
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("1:35"));
    assert!(
        app.sync.take_wanted().is_some(),
        "a sync brings what the phone wrote"
    );
}

#[gpui_kit::test]
fn a_command_that_finds_nothing_active_leaves_remote_mode(cx: &mut TestAppContext) {
    let (app, stub) = open(
        cx,
        false,
        Some(playback(Some(PHONE), EPISODE, 60_000, true)),
    );
    app.wait_for_label(cx, "player-where", "Playing on Pixel 8");
    let _ = app.sync.take_wanted();
    stub.with(|account| {
        account.fail_command = Some((StatusCode::CONFLICT, ErrorCode::NoSession));
    });
    click(&app, cx, "player-toggle");
    app.wait_until(cx, "remote mode ends", |window| {
        window.try_find("player-where").is_none()
    });
    assert!(app.sync.take_wanted().is_some());
}

// ==============================================================================
// Tests: nothing active, and the picker
// ==============================================================================

#[gpui_kit::test]
fn the_last_playback_replaces_the_restored_episode(cx: &mut TestAppContext) {
    let (app, _stub) = open(cx, false, Some(playback(None, UNKNOWN, 42_000, false)));
    app.wait_for_label(cx, "player-title", "EP 152: Elsewhere");
    assert!(!shows(&app, cx, "player-where"), "nothing plays elsewhere");
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("0:42"));
}

#[gpui_kit::test]
fn the_picker_lists_this_device_the_others_and_every_group(cx: &mut TestAppContext) {
    let (app, _stub) = open(cx, false, None);
    app.wait_until(cx, "the picker shows", |window| {
        window.try_find("player-output").is_some()
    });
    click(&app, cx, "player-output");
    app.wait_for_label(cx, &format!("output-{COUCH}"), "Couch");
    app.wait_for_label(
        cx,
        &format!("output-{KITCHEN}"),
        "Kitchen, playing something else",
    );
    cx.update_window(app.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("output-local").label(), Some("This device"));
        assert_eq!(window.find("output-local").selected(), Some(true));
        assert_eq!(
            window.find(format!("output-{PHONE}")).label(),
            Some("Pixel 8")
        );
        assert!(
            window.try_find(format!("output-{OWN}")).is_none(),
            "this device is listed once"
        );
        assert_eq!(
            window.find(format!("output-{COUCH}")).selected(),
            Some(false)
        );
    })
    .expect("the window is still open");
}

#[gpui_kit::test]
fn the_picker_transfers_a_local_play_to_a_group(cx: &mut TestAppContext) {
    let (mut app, stub) = playing_here(cx);
    click(&app, cx, "player-output");
    app.wait_for_label(cx, &format!("output-{COUCH}"), "Couch");
    click(&app, cx, &format!("output-{COUCH}"));

    assert_eq!(
        next_command(&mut app, cx),
        Command::Pause(true),
        "the release"
    );
    assert_eq!(next_command(&mut app, cx), Command::Stop);
    app.wait_for_label(cx, "player-where", "Playing on Couch");
    assert_eq!(
        stub.bodies("/v1/connect/transfer"),
        [json!({ "device_id": COUCH })]
    );
    let released = stub.reports().pop().expect("the release was reported");
    assert!(released["released"].is_string(), "{released}");
    assert_eq!(released["position_ms"], 760_000);
    assert!(!shows(&app, cx, "player-speed"));
    wait_for(&app, cx, "one pause write", || {
        positions(&app) == [json!(760_000)]
    });
}

#[gpui_kit::test]
fn this_device_takes_the_playback_back_through_a_transfer(cx: &mut TestAppContext) {
    let (mut app, stub) = open(
        cx,
        false,
        Some(playback(Some(COUCH), EPISODE, 800_000, true)),
    );
    app.wait_for_label(cx, "player-where", "Playing on Couch");
    click(&app, cx, "player-output");
    app.wait_for_label(cx, "output-local", "This device");
    click(&app, cx, "output-local");
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: STREAM.to_owned(),
            start_ms: 800_000,
            paused: false,
        },
        "playing on, from where the group was"
    );
    app.wait_until(cx, "remote mode ends", |window| {
        window.try_find("player-where").is_none()
    });
    assert_eq!(
        stub.bodies("/v1/connect/transfer"),
        [json!({ "device_id": OWN })]
    );
    wait_for(&app, cx, "the start is reported", || {
        stub.reports()
            .last()
            .is_some_and(|report| report["playing"] == true)
    });
}

#[gpui_kit::test]
fn a_transfer_that_fails_shows_a_notification(cx: &mut TestAppContext) {
    let (app, stub) = open(cx, false, Some(playback(None, EPISODE, 754_000, false)));
    stub.with(|account| {
        account.fail_transfer = Some((StatusCode::BAD_GATEWAY, ErrorCode::UpstreamUnavailable));
    });
    app.wait_until(cx, "the picker shows", |window| {
        window.try_find("player-output").is_some()
    });
    click(&app, cx, "player-output");
    app.wait_for_label(cx, &format!("output-{COUCH}"), "Couch");
    click(&app, cx, &format!("output-{COUCH}"));
    app.wait_for_label(cx, "connect-notice", "Couldn't reach Couch.");
    assert!(!shows(&app, cx, "player-where"));
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("12:34"));
}
