//! Sonos playback in the real window (spec 13.4), against a stub pod-server
//! that plays the speakers' part: it keeps one session per group the way
//! pod-server does, and records every request.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse as _, Response};
use color_eyre::eyre::{Result, WrapErr as _};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use pod_protocol::{
    ErrorBody, ErrorCode, SonosCommand, SonosPlayRequest, SonosPlayer, SonosPlayerState,
    SonosPlayersResponse, SonosReleased, SonosSessionView,
};
use rusqlite::{Transaction, params};
use tokio::sync::{oneshot, watch};

use crate::app::tests::{TestApp, signed_in};
use crate::player::LAST_EPISODE_ID;
use crate::player::mpv::{Command, Event};
use crate::test_port::bind_test_port;

const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
const EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c01";
const COUCH: &str = "RINCON_804AF246458401400";
const KITCHEN: &str = "RINCON_B8E937000000001400";
const DURATION_MS: i64 = 3_120_000;
const HOUR_MS: i64 = 60 * 60 * 1000;

// ==============================================================================
// The stub server
// ==============================================================================

/// What the stub's groups are doing, and what it heard.
#[derive(Debug)]
struct Speakers {
    players: Vec<SonosPlayer>,
    /// `METHOD /path body`, oldest first, polls of the list left out.
    requests: Vec<String>,
    /// Answers the next play with this status instead of starting.
    fail_play: Option<StatusCode>,
    /// Answers the next command with this status and code.
    fail_command: Option<(StatusCode, ErrorCode)>,
}

impl Speakers {
    fn player(&mut self, id: &str) -> &mut SonosPlayer {
        self.players
            .iter_mut()
            .find(|player| player.id == id)
            .expect("the stub has the group")
    }
}

/// A pod-server with two groups: "Couch", free, and "Kitchen", playing
/// something else. Holds every play request until [`Stub::release_play`]
/// while `hold_play` is set, and answers syncs with `503`.
struct Stub {
    speakers: Arc<Mutex<Speakers>>,
    plays: watch::Sender<bool>,
    _stop: oneshot::Sender<()>,
}

impl Stub {
    /// Serves on a port from [`bind_test_port`] (`POD_TEST_SONOS_UI_PORT`
    /// overrides the choice) and points the seeded account at the stub.
    fn start(app: &TestApp, hold_play: bool) -> Stub {
        let listener = bind_test_port("POD_TEST_SONOS_UI_PORT");
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
        let speakers = Arc::new(Mutex::new(Speakers {
            players: vec![
                group(COUCH, "Couch", SonosPlayerState::Idle),
                group(KITCHEN, "Kitchen", SonosPlayerState::Other),
            ],
            requests: Vec::new(),
            fail_play: None,
            fail_command: None,
        }));
        let (plays, released) = watch::channel(!hold_play);
        let shared = speakers.clone();
        let router = axum::Router::new().fallback(move |request: axum::extract::Request| {
            let speakers = shared.clone();
            let mut released = released.clone();
            async move {
                let (method, path) = (request.method().clone(), request.uri().path().to_owned());
                let authorized = request
                    .headers()
                    .get("authorization")
                    .is_some_and(|value| value == "Bearer secret");
                let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                    .await
                    .expect("the body arrives");
                assert!(authorized, "{method} {path} carries the token");
                if method != Method::GET {
                    speakers
                        .lock()
                        .expect("not poisoned")
                        .requests
                        .push(format!(
                            "{method} {path} {}",
                            String::from_utf8_lossy(&body)
                        ));
                }
                if path.ends_with("/play") {
                    let _ = released.wait_for(|released| *released).await;
                }
                answer(&mut speakers.lock().expect("not poisoned"), &path, &body)
            }
        });
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
            speakers,
            plays,
            _stop: stop,
        }
    }

    fn release_play(&self) {
        self.plays.send_replace(true);
    }

    fn with<T>(&self, change: impl FnOnce(&mut Speakers) -> T) -> T {
        change(&mut self.speakers.lock().expect("not poisoned"))
    }

    fn requests(&self) -> Vec<String> {
        self.with(|speakers| speakers.requests.clone())
    }

    /// The requests to the Sonos endpoints, without the `/v1/sonos/players/`
    /// prefix and the group id: `play {…}`, `command {…}`, `release `.
    fn sonos_requests(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter_map(|request| {
                let rest = request.strip_prefix("POST /v1/sonos/players/")?;
                let (_, rest) = rest.split_once('/')?;
                Some(rest.to_owned())
            })
            .collect()
    }
}

impl Drop for Stub {
    /// A held play would keep the graceful shutdown waiting.
    fn drop(&mut self) {
        self.release_play();
    }
}

/// Plays pod-server's part for one request (spec 13.2 and 13.3).
fn answer(speakers: &mut Speakers, path: &str, body: &[u8]) -> Response {
    let error = |status: StatusCode, code: ErrorCode| {
        (status, axum::Json(ErrorBody::new(code, "the stub says no"))).into_response()
    };
    if path == "/v1/sync" {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if path == "/v1/sonos/players" {
        return axum::Json(SonosPlayersResponse {
            players: speakers.players.clone(),
        })
        .into_response();
    }
    let rest = path
        .strip_prefix("/v1/sonos/players/")
        .expect("only Sonos and sync requests reach the stub");
    let (id, action) = rest.split_once('/').expect("a group action");
    match action {
        "play" => {
            if let Some(status) = speakers.fail_play.take() {
                return error(status, ErrorCode::UpstreamUnavailable);
            }
            let request: SonosPlayRequest = serde_json::from_slice(body).expect("a play request");
            let player = speakers.player(id);
            player.state = SonosPlayerState::Pod;
            let mut session = SonosSessionView {
                episode_id: request.episode_id,
                playing: false,
                position_ms: request.position_ms,
            };
            player.session = Some(session);
            // The answer says it is still buffering, as a speaker just told
            // to play does; the polls after it see it playing. The stub's
            // speakers have no clock: the position stands.
            let answer = axum::Json(player.clone()).into_response();
            session.playing = true;
            player.session = Some(session);
            answer
        }
        "command" => {
            if let Some((status, code)) = speakers.fail_command.take() {
                return error(status, code);
            }
            let command: SonosCommand = serde_json::from_slice(body).expect("a command");
            let player = speakers.player(id);
            if let SonosCommand::Volume { volume } = command {
                player.volume = volume;
                return axum::Json(player.clone()).into_response();
            }
            let Some(session) = &mut player.session else {
                return error(StatusCode::CONFLICT, ErrorCode::NoSession);
            };
            match command {
                SonosCommand::Pause => session.playing = false,
                SonosCommand::Resume => session.playing = true,
                SonosCommand::Seek { position_ms } => session.position_ms = position_ms,
                SonosCommand::Skip { delta_ms } => {
                    session.position_ms = (session.position_ms + delta_ms).max(0);
                }
                SonosCommand::Volume { .. } => {}
            }
            axum::Json(player.clone()).into_response()
        }
        "release" => {
            let player = speakers.player(id);
            let Some(session) = player.session.take() else {
                return error(StatusCode::CONFLICT, ErrorCode::NoSession);
            };
            player.state = SonosPlayerState::Idle;
            axum::Json(SonosReleased {
                episode_id: session.episode_id,
                position_ms: session.position_ms,
                playing: session.playing,
            })
            .into_response()
        }
        other => panic!("no Sonos action {other}"),
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
                             {DURATION_MS}, 'https://example.com/pager.mp3', 'audio/mpeg', 1);",
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
fn outbox(app: &TestApp) -> Vec<serde_json::Value> {
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

/// Opens the window over [`seed`] with the stub, and waits until the
/// picker's button shows: the server listed groups.
fn open(cx: &mut TestAppContext, played: bool, hold_play: bool) -> (TestApp, Stub) {
    let app = TestApp::open(cx, seed(played));
    let stub = Stub::start(&app, hold_play);
    app.wait_for_label(cx, "player-title", "EP 151: The pager");
    app.wait_until(cx, "the picker shows", |window| {
        window.try_find("player-output").is_some()
    });
    (app, stub)
}

/// Hands the episode to Couch through the picker.
fn hand_to_couch(app: &TestApp, cx: &mut TestAppContext) {
    click(app, cx, "player-output");
    app.wait_for_label(cx, &format!("output-{COUCH}"), "Couch");
    click(app, cx, &format!("output-{COUCH}"));
}

/// [`open`], then hands off to Couch and waits until it plays there.
fn playing_on_couch(cx: &mut TestAppContext) -> (TestApp, Stub) {
    let (app, stub) = open(cx, false, false);
    hand_to_couch(&app, cx);
    app.wait_for_label(cx, "player-where", "Playing on Couch");
    (app, stub)
}

// ==============================================================================
// Tests
// ==============================================================================

#[gpui_kit::test]
fn the_picker_lists_this_device_and_every_group(cx: &mut TestAppContext) {
    let (app, _stub) = open(cx, false, false);
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
            window.find(format!("output-{COUCH}")).selected(),
            Some(false)
        );
        assert!(window.find("player-speed").visible());
    })
    .expect("the window is still open");
}

#[gpui_kit::test]
fn handing_off_stops_the_local_play_and_starts_the_group_where_it_stood(cx: &mut TestAppContext) {
    let (mut app, stub) = open(cx, false, true);
    click(&app, cx, "player-toggle");
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: "https://example.com/pager.mp3".to_owned(),
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

    hand_to_couch(&app, cx);
    assert_eq!(next_command(&mut app, cx), Command::Stop);
    app.wait_for_label(cx, "player-where", "Connecting to Couch…");
    assert!(!shows(&app, cx, "player-speed"), "Sonos plays at 1x only");
    wait_for(&app, cx, "the play request arrives", || {
        !stub.sonos_requests().is_empty()
    });
    // Commands wait for the group's answer.
    click(&app, cx, "player-toggle");

    stub.release_play();
    app.wait_for_label(cx, "player-where", "Playing on Couch");
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Pause"));
    assert_eq!(
        label(&app, cx, "player-volume").as_deref(),
        Some("Volume on Couch")
    );
    assert!(!shows(&app, cx, "player-speed"));
    assert_eq!(
        stub.sonos_requests(),
        [format!(
            r#"play {{"episode_id":"{EPISODE}","position_ms":760000}}"#
        )],
        "the toggle while connecting sent nothing"
    );

    let requests = stub.requests();
    let sync = requests
        .iter()
        .position(|request| request.starts_with("POST /v1/sync"))
        .expect("the outbox was pushed");
    assert!(
        requests[sync].contains(r#""position_ms":760000"#),
        "{}",
        requests[sync]
    );
    assert!(sync < requests.len() - 1, "before the play request");
    let positions: Vec<_> = outbox(&app)
        .iter()
        .filter(|mutation| mutation["type"] == "episode.position")
        .map(|mutation| mutation["position_ms"].clone())
        .collect();
    assert_eq!(positions, [serde_json::json!(760_000)], "one stop write");
    assert!(
        app.mpv_commands.try_recv().is_err(),
        "mpv hears nothing more"
    );
}

#[gpui_kit::test]
fn a_played_episode_starts_over_on_the_group_and_is_marked_unplayed_first(cx: &mut TestAppContext) {
    let (app, stub) = open(cx, true, false);
    hand_to_couch(&app, cx);
    app.wait_for_label(cx, "player-where", "Playing on Couch");
    assert_eq!(
        stub.sonos_requests(),
        [format!(
            r#"play {{"episode_id":"{EPISODE}","position_ms":0}}"#
        )]
    );
    let requests = stub.requests();
    assert!(
        requests[0].starts_with("POST /v1/sync") && requests[0].contains(r#""played":false"#),
        "the replay reset is pushed first: {requests:?}"
    );
}

#[gpui_kit::test]
fn the_buttons_steer_the_group_and_write_nothing_here(cx: &mut TestAppContext) {
    let (mut app, stub) = playing_on_couch(cx);
    let outbox_before = outbox(&app).len();

    // One at a time, so they reach the stub in order.
    for (button, sent) in [
        ("player-toggle", 2),
        ("player-skip-forward", 3),
        ("player-skip-back", 4),
        ("player-volume", 5),
    ] {
        click(&app, cx, button);
        wait_for(&app, cx, &format!("{button}'s command arrives"), || {
            stub.sonos_requests().len() == sent
        });
    }
    app.wait_for_label(cx, "player-toggle", "Play");
    assert_eq!(
        stub.sonos_requests()[1..],
        [
            r#"command {"type":"pause"}"#,
            r#"command {"type":"skip","delta_ms":30000}"#,
            r#"command {"type":"skip","delta_ms":-15000}"#,
            r#"command {"type":"volume","volume":50}"#,
        ]
    );
    app.wait_until(cx, "the slider shows the new volume", |window| {
        window
            .try_find("player-volume")
            .is_some_and(|slider| slider.value() == Some("50"))
    });
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("12:49"));
    assert_eq!(
        outbox(&app).len(),
        outbox_before,
        "the server writes, not this device"
    );
    assert!(app.mpv_commands.try_recv().is_err(), "mpv stays stopped");
}

#[gpui_kit::test]
fn a_409_leaves_remote_mode_and_syncs(cx: &mut TestAppContext) {
    let (app, stub) = playing_on_couch(cx);
    let _ = app.sync.take_wanted();
    stub.with(|speakers| {
        speakers.fail_command = Some((StatusCode::CONFLICT, ErrorCode::NoSession));
    });
    click(&app, cx, "player-toggle");
    app.wait_until(cx, "remote mode ends", |window| {
        window.try_find("player-where").is_none()
    });
    assert!(shows(&app, cx, "player-speed"));
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Play"));
    assert!(
        app.sync.take_wanted().is_some(),
        "a sync brings what the server wrote"
    );
}

#[gpui_kit::test]
fn the_group_playing_something_else_ends_remote_mode(cx: &mut TestAppContext) {
    let (app, stub) = playing_on_couch(cx);
    stub.with(|speakers| {
        let couch = speakers.player(COUCH);
        couch.state = SonosPlayerState::Other;
        couch.session = None;
    });
    // The next poll, within 2 s while the window is active.
    app.wait_until(cx, "remote mode ends", |window| {
        window.try_find("player-where").is_none()
    });
    assert!(shows(&app, cx, "player-speed"));
    assert!(app.sync.take_wanted().is_some());
}

#[gpui_kit::test]
fn this_device_takes_the_episode_back_where_the_group_left_it(cx: &mut TestAppContext) {
    let (mut app, stub) = playing_on_couch(cx);
    stub.with(|speakers| {
        let session = speakers
            .player(COUCH)
            .session
            .as_mut()
            .expect("Couch plays the episode");
        session.playing = true;
        session.position_ms = 800_000;
    });
    click(&app, cx, "player-output");
    app.wait_for_label(cx, "output-local", "This device");
    click(&app, cx, "output-local");
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: "https://example.com/pager.mp3".to_owned(),
            start_ms: 800_000,
            paused: false,
        },
        "playing on, from where the group was"
    );
    app.wait_until(cx, "remote mode ends", |window| {
        window.try_find("player-where").is_none()
    });
    assert!(shows(&app, cx, "player-speed"));
    assert_eq!(
        stub.sonos_requests().last().map(String::as_str),
        Some("release ")
    );
    let played: Vec<_> = outbox(&app)
        .iter()
        .filter(|mutation| mutation["type"] == "episode.played")
        .cloned()
        .collect();
    assert_eq!(played, Vec::<serde_json::Value>::new(), "not a replay");
}

#[gpui_kit::test]
fn a_speaker_that_does_not_answer_shows_a_notification(cx: &mut TestAppContext) {
    let (app, stub) = open(cx, false, false);
    stub.with(|speakers| speakers.fail_play = Some(StatusCode::BAD_GATEWAY));
    hand_to_couch(&app, cx);
    app.wait_for_label(cx, "sonos-notice", "Couldn't reach Couch.");
    assert!(!shows(&app, cx, "player-where"));
    assert!(shows(&app, cx, "player-speed"));
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("12:34"));
}
