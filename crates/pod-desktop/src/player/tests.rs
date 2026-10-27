//! The player in the real window, with the tests playing mpv's part over
//! the engine's channels (`TestApp::mpv_commands` and `mpv_events`).

use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr as _};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use rusqlite::{OptionalExtension as _, Transaction, params};

use super::media_keys::{self, Remote};
use super::mpv::{Command, Event, Failure};
use super::{LAST_EPISODE_ID, PlayerHandle};
use crate::app::tests::TestApp;
use crate::reconcile::Reconciler;
use crate::sync::session::Session;

const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
const EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c01";
const STREAM: &str = "https://example.com/pager.mp3";
/// Where the episode's download is published, relative to the root.
const DOWNLOAD: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c/pager.run.mp3";
const DURATION_MS: i64 = 3_120_000;
const HOUR_MS: i64 = 60 * 60 * 1000;

/// A signed-in account subscribed to one podcast with one inbox episode,
/// started at 12:34.
fn seed(transaction: &Transaction<'_>) -> Result<()> {
    let now = pod_protocol::Timestamp::now().as_epoch_ms();
    crate::app::tests::signed_in(transaction)?;
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
                         {DURATION_MS}, '{STREAM}', 'audio/mpeg', 1);
             insert into episode_state (episode_id, podcast_id, position_ms, position_at,
                     position_dev)
                 values ('{EPISODE}', '{PODCAST}', 754000, 1, 'dev');",
            subscribed = now - 240 * HOUR_MS,
            published = now - HOUR_MS,
        ))
        .context("insert the podcast and episode")
}

/// [`seed`], with the episode remembered as the last one played.
fn seed_remembered(transaction: &Transaction<'_>) -> Result<()> {
    seed(transaction)?;
    transaction
        .execute(
            "insert into device_setting (key, value) values (?1, ?2)",
            params![LAST_EPISODE_ID, format!("\"{EPISODE}\"")],
        )
        .context("remember the episode")?;
    Ok(())
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

fn send(app: &TestApp, events: impl IntoIterator<Item = Event>) {
    for event in events {
        app.mpv_events
            .send(event)
            .expect("the player listens while the window is open");
    }
}

/// `(position_ms, played)` of the episode's state.
fn state(app: &TestApp) -> (i64, bool) {
    app.read(|connection| {
        connection
            .query_row(
                "select position_ms, played from episode_state where episode_id = ?1",
                [EPISODE],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("read the episode state")
    })
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

fn remembered(app: &TestApp) -> Option<String> {
    app.read(|connection| {
        connection
            .query_row(
                "select value from device_setting where key = ?1",
                [LAST_EPISODE_ID],
                |row| row.get(0),
            )
            .optional()
            .context("read the last episode id")
    })
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

fn click(app: &TestApp, cx: &mut TestAppContext, id: &str) {
    cx.update_window(app.window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id.to_owned(), cx);
    })
    .expect("the window is still open");
}

#[gpui_kit::test]
fn finishing_an_episode_marks_it_played_and_clears_it_from_the_inbox(cx: &mut TestAppContext) {
    let mut app = TestApp::open(cx, |transaction| {
        seed(transaction)?;
        transaction
            .execute(
                "insert into download (episode_id, state, audio_url, path, completed_at)
                 values (?1, 'complete', ?2, ?3, 1)",
                params![EPISODE, STREAM, DOWNLOAD],
            )
            .context("insert the download")?;
        Ok(())
    });
    let file = cx.update(|cx| cx.global::<Reconciler>().root().join(DOWNLOAD));
    std::fs::create_dir_all(file.parent().expect("the path has a podcast directory"))
        .expect("the downloads root is writable");
    std::fs::write(&file, b"audio").expect("the downloads root is writable");
    let row = format!("inbox-row-{EPISODE}");
    app.wait_until(cx, "the inbox lists the episode", |window| {
        window.try_find(row.clone()).is_some()
    });
    assert_eq!(label(&app, cx, "player-title"), None, "nothing to restore");

    cx.update_window(app.window, |_, window, cx| window.press("enter", cx))
        .expect("the window is still open");
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: file.display().to_string(),
            start_ms: 754_000,
            paused: false,
        },
        "a complete download plays from disk, at the stored position"
    );
    app.wait_until(cx, "the player bar shows the episode", |window| {
        window.try_find("player-title").is_some()
    });
    assert_eq!(
        label(&app, cx, "player-title").as_deref(),
        Some("EP 151: The pager")
    );
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("12:34"));
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Pause"));

    send(
        &app,
        [
            // The end of the file it replaced.
            Event::Ended,
            Event::Started,
            Event::Duration(DURATION_MS),
            Event::Position(DURATION_MS - 20_000),
        ],
    );
    app.wait_until(cx, "the threshold marked the episode played", |_| {
        state(&app).1
    });
    assert_eq!(
        label(&app, cx, "player-toggle").as_deref(),
        Some("Pause"),
        "the replaced file's end, before the start, is ignored"
    );
    send(&app, [Event::Ended]);
    app.wait_until(cx, "the end saved the position", |_| {
        state(&app).0 == DURATION_MS
    });
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Play"));
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("52:00"));
    // The store has the flag; the inbox's live query delivers it on its own.
    app.wait_until(cx, "the inbox drops the played episode", |window| {
        window.try_find(row.clone()).is_none()
    });

    let types: Vec<_> = outbox(&app)
        .iter()
        .map(|mutation| (mutation["type"].clone(), mutation.get("played").cloned()))
        .collect();
    assert_eq!(
        types,
        [
            (
                serde_json::json!("episode.played"),
                Some(serde_json::json!(true))
            ),
            (serde_json::json!("episode.position"), None),
        ],
        "the threshold marks it once"
    );
    assert_eq!(remembered(&app), Some(format!("\"{EPISODE}\"")));
}

/// The bar shows the podcast's cached artwork with the network off: the
/// player's episode carries the `artwork.file` its load joined in.
#[gpui_kit::test]
fn the_player_bar_shows_cached_artwork_offline(cx: &mut TestAppContext) {
    let art = "https://img.example.com/darknet.jpg";
    let app = TestApp::open(cx, move |transaction| {
        seed_remembered(transaction)?;
        transaction
            .execute(
                "update podcast set artwork_url = ?1 where id = ?2",
                params![art, PODCAST],
            )
            .context("give the podcast artwork")?;
        transaction
            .execute(
                "insert into artwork (url, file, bytes, last_used) values (?1, 'darknet', 1, 1)",
                [art],
            )
            .context("cache the artwork")?;
        Ok(())
    });
    app.network.send_replace(crate::net::NetworkState {
        connected: false,
        metered: false,
    });
    let expected = cx.update(|cx| {
        cx.global::<Reconciler>()
            .artwork_root()
            .join("darknet")
            .display()
            .to_string()
    });
    app.wait_until(cx, "the player bar shows the cached artwork", |window| {
        window
            .try_find("player-artwork")
            .is_some_and(|element| element.value() == Some(expected.as_str()))
    });
}

#[gpui_kit::test]
fn a_cold_start_restores_the_last_episode_paused(cx: &mut TestAppContext) {
    let mut app = TestApp::open(cx, seed_remembered);
    app.wait_until(cx, "the player bar restored the episode", |window| {
        window.try_find("player-title").is_some()
    });
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("12:34"));
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Play"));
    assert!(
        app.mpv_commands.try_recv().is_err(),
        "restoring loads nothing into mpv"
    );

    click(&app, cx, "player-skip-back");
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("12:19"));
    click(&app, cx, "player-toggle");
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: STREAM.to_owned(),
            start_ms: 739_000,
            paused: false,
        },
        "without a download it streams, from the position the skip saved"
    );

    // A sync deletes the episode while it plays; the player carries on.
    app.runtime
        .block_on(app.store.write(|transaction| {
            transaction
                .execute("delete from episode where id = ?1", [EPISODE])
                .context("delete the episode")
        }))
        .expect("the delete commits");

    send(&app, [Event::Started, Event::Position(745_000)]);
    app.wait_for_label(cx, "player-position", "12:25");
    click(&app, cx, "player-skip-forward");
    assert_eq!(next_command(&mut app, cx), Command::Seek(775_000));
    click(&app, cx, "player-toggle");
    assert_eq!(next_command(&mut app, cx), Command::Pause(true));
    app.wait_until(cx, "the pause saved the position", |_| {
        state(&app) == (775_000, false)
    });
    assert_eq!(
        label(&app, cx, "player-title").as_deref(),
        Some("EP 151: The pager"),
        "the bar keeps the episode it started with"
    );

    // Quitting saves the position once more and waits for it.
    let player = cx.update(|cx| cx.global::<PlayerHandle>().0.clone());
    let worker = cx
        .update(|cx| player.update(cx, |player, _| player.quit()))
        .expect("the worker runs until the quit");
    app.runtime
        .block_on(worker)
        .expect("the worker finishes without panicking");
    let positions: Vec<_> = outbox(&app)
        .iter()
        .filter(|mutation| mutation["type"] == "episode.position")
        .map(|mutation| mutation["position_ms"].clone())
        .collect();
    assert_eq!(
        positions,
        [serde_json::json!(775_000)],
        "position writes coalesce into one mutation"
    );
}

#[gpui_kit::test]
fn a_failed_play_says_why_in_the_bar_and_retry_plays_again(cx: &mut TestAppContext) {
    let mut app = TestApp::open(cx, seed_remembered);
    app.wait_until(cx, "the player bar restored the episode", |window| {
        window.try_find("player-title").is_some()
    });
    let load = Command::Load {
        source: STREAM.to_owned(),
        start_ms: 754_000,
        paused: false,
    };
    click(&app, cx, "player-toggle");
    assert_eq!(next_command(&mut app, cx), load);
    send(
        &app,
        [Event::Started, Event::Failed(Failure::NoAudioOutput)],
    );
    app.wait_for_label(cx, "player-failure", "Couldn't play: no audio output");
    assert_eq!(label(&app, cx, "player-toggle").as_deref(), Some("Play"));
    assert_eq!(
        label(&app, cx, "player-position"),
        None,
        "the message takes the timeline's place"
    );

    click(&app, cx, "player-retry");
    assert_eq!(next_command(&mut app, cx), load, "retry loads it again");
    send(&app, [Event::Started, Event::Failed(Failure::Unloadable)]);
    app.wait_for_label(cx, "player-failure", "Couldn't load the episode");

    click(&app, cx, "player-toggle");
    assert_eq!(next_command(&mut app, cx), load, "Play retries too");
    send(&app, [Event::Started, Event::Position(755_000)]);
    app.wait_for_label(cx, "player-position", "12:35");
    assert_eq!(
        label(&app, cx, "player-failure"),
        None,
        "a play that starts clears the failure"
    );
}

#[gpui_kit::test]
fn signing_out_stops_playback_and_clears_the_player_and_search(cx: &mut TestAppContext) {
    let mut app = TestApp::open(cx, seed_remembered);
    app.wait_until(cx, "the player bar restored the episode", |window| {
        window.try_find("player-title").is_some()
    });
    click(&app, cx, "player-toggle");
    assert!(matches!(next_command(&mut app, cx), Command::Load { .. }));
    send(&app, [Event::Started, Event::Position(755_000)]);
    app.wait_for_label(cx, "player-position", "12:35");
    cx.update_window(app.window, |_, window, cx| {
        window.click("search-field", cx);
        window.input("feeds.example.com/show.xml", cx);
    })
    .expect("the window is still open");
    app.wait_until(cx, "the field holds the query", |window| {
        window.find("search-field").value() == Some("feeds.example.com/show.xml")
    });

    let session = cx.update(|cx| cx.global::<Session>().clone());
    app.runtime
        .block_on(session.logout())
        .expect("logout works");
    app.wait_until(cx, "the sign-in card shows", |window| {
        window.try_find("player-bar").is_none()
    });
    assert_eq!(
        next_command(&mut app, cx),
        Command::Stop,
        "mpv lets go of the old account's file"
    );
    assert_eq!(
        remembered(&app),
        None,
        "a restart doesn't restore it either"
    );

    app.runtime
        .block_on(app.store.write(crate::app::tests::signed_in))
        .expect("the sign-in commits");
    app.wait_until(cx, "the app shows again", |window| {
        window.try_find("player-bar").is_some()
    });
    assert_eq!(label(&app, cx, "player-title"), None, "the player is empty");
    app.wait_until(cx, "the field is empty", |window| {
        window
            .find("search-field")
            .value()
            .unwrap_or_default()
            .is_empty()
    });
    assert!(
        app.mpv_commands.try_recv().is_err(),
        "nothing else reached mpv"
    );
}

#[gpui_kit::test]
fn replaying_a_played_episode_starts_over_and_unmarks_it(cx: &mut TestAppContext) {
    let mut app = TestApp::open(cx, |transaction| {
        seed_remembered(transaction)?;
        transaction
            .execute(
                "update episode_state set played = 1, played_at = 1, played_dev = 'dev'",
                [],
            )
            .context("mark the episode played")?;
        Ok(())
    });
    app.wait_until(cx, "the player bar restored the episode", |window| {
        window.try_find("player-title").is_some()
    });
    assert_eq!(label(&app, cx, "player-position").as_deref(), Some("0:00"));
    click(&app, cx, "player-toggle");
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: STREAM.to_owned(),
            start_ms: 0,
            paused: false,
        }
    );
    app.wait_until(cx, "the replay unmarked the episode", |_| !state(&app).1);
    let played: Vec<_> = outbox(&app)
        .iter()
        .map(|mutation| (mutation["type"].clone(), mutation["played"].clone()))
        .collect();
    assert_eq!(
        played,
        [(
            serde_json::json!("episode.played"),
            serde_json::json!(false)
        )]
    );
}

#[gpui_kit::test]
fn speed_follows_the_synced_setting_and_the_chip_writes_it(cx: &mut TestAppContext) {
    let mut app = TestApp::open(cx, |transaction| {
        seed_remembered(transaction)?;
        transaction
            .execute(
                "insert into setting (key, value, value_at, value_dev)
                 values ('playback_speed', '1.5', 1, 'dev')",
                [],
            )
            .context("insert the speed")?;
        Ok(())
    });
    assert_eq!(next_command(&mut app, cx), Command::Speed(1.5));
    app.wait_until(cx, "the player bar restored the episode", |window| {
        window.try_find("player-title").is_some()
    });

    click(&app, cx, "player-speed");
    assert_eq!(next_command(&mut app, cx), Command::Speed(1.75));
    let speed = |app: &TestApp| {
        app.read(|connection| {
            connection
                .query_row(
                    "select value from setting where key = 'playback_speed'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .context("read the speed")
        })
    };
    app.wait_until(cx, "the speed setting was written", |_| {
        speed(&app) == "1.75"
    });
    assert_eq!(
        outbox(&app)
            .iter()
            .map(|mutation| mutation["type"].clone())
            .collect::<Vec<_>>(),
        [serde_json::json!("setting.set")]
    );
    assert!(
        app.mpv_commands.try_recv().is_err(),
        "the store's echo of the new speed changes nothing more"
    );
}

#[gpui_kit::test]
fn media_controls_drive_the_player(cx: &mut TestAppContext) {
    let mut app = TestApp::open(cx, seed_remembered);
    app.wait_until(cx, "the player bar restored the episode", |window| {
        window.try_find("player-title").is_some()
    });
    let player = cx.update(|cx| cx.global::<PlayerHandle>().0.clone());
    let remote = |cx: &mut TestAppContext, remote: Remote| {
        cx.update(|cx| player.update(cx, |player, cx| media_keys::apply(player, remote, cx)));
    };

    remote(cx, Remote::SeekTo(600_000));
    assert_eq!(
        label(&app, cx, "player-position").as_deref(),
        Some("10:00"),
        "a seek before playing moves the restored position"
    );
    remote(cx, Remote::Play);
    assert_eq!(
        next_command(&mut app, cx),
        Command::Load {
            source: STREAM.to_owned(),
            start_ms: 600_000,
            paused: false,
        }
    );
    send(&app, [Event::Started, Event::Position(745_000)]);
    app.wait_for_label(cx, "player-position", "12:25");

    remote(cx, Remote::Play);
    remote(cx, Remote::SeekBy(-5_000));
    assert_eq!(
        next_command(&mut app, cx),
        Command::Seek(740_000),
        "Play while playing does nothing"
    );
    remote(cx, Remote::SeekBy(-HOUR_MS));
    assert_eq!(next_command(&mut app, cx), Command::Seek(0));
    remote(cx, Remote::SeekTo(2 * HOUR_MS));
    assert_eq!(
        next_command(&mut app, cx),
        Command::Seek(DURATION_MS),
        "seeks stop at the end"
    );
    remote(cx, Remote::SeekTo(60_000));
    assert_eq!(next_command(&mut app, cx), Command::Seek(60_000));
    remote(cx, Remote::Skip { forward: true });
    assert_eq!(next_command(&mut app, cx), Command::Seek(90_000));
    remote(cx, Remote::Skip { forward: false });
    assert_eq!(next_command(&mut app, cx), Command::Seek(75_000));
    remote(cx, Remote::Toggle);
    assert_eq!(next_command(&mut app, cx), Command::Pause(true));
    remote(cx, Remote::Pause);
    remote(cx, Remote::Play);
    assert_eq!(
        next_command(&mut app, cx),
        Command::Pause(false),
        "Pause while paused does nothing"
    );
    remote(cx, Remote::Pause);
    assert_eq!(next_command(&mut app, cx), Command::Pause(true));
}
