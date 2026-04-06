use std::sync::Arc;

use anyhow::Context;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MediaKeyCode};
use tokio::sync::Mutex;

use crate::tui::api_client::ApiClient;
use crate::tui::app::{Action, App, View};
use crate::tui::local_db::LocalDb;
use crate::tui::player::{PlaybackState, Player};

/// Shared handle to the mpv player, accessible from the event handler and
/// the periodic poll task.
pub type PlayerHandle = Arc<Mutex<Option<Player>>>;

/// Map a key event to an action based on the current view.
pub fn map_key(app: &App, key: KeyEvent) -> Option<Action> {
    // Ctrl+C always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }

    // Media keys work regardless of whether something is playing — mpv
    // handles the no-op case gracefully.
    match key.code {
        KeyCode::Media(MediaKeyCode::PlayPause) | KeyCode::Media(MediaKeyCode::Play) | KeyCode::Media(MediaKeyCode::Pause) => {
            return Some(Action::TogglePause);
        }
        KeyCode::Media(MediaKeyCode::Stop) => return Some(Action::StopPlayback),
        KeyCode::Media(MediaKeyCode::FastForward) => return Some(Action::SeekForward),
        KeyCode::Media(MediaKeyCode::Rewind) => return Some(Action::SeekBackward),
        _ => {}
    }

    // Global playback controls (available in any view when playing).
    if app.now_playing.is_some() {
        match key.code {
            KeyCode::Char(' ') => return Some(Action::TogglePause),
            KeyCode::Char('s') => return Some(Action::StopPlayback),
            KeyCode::Right => return Some(Action::SeekForward),
            KeyCode::Left => return Some(Action::SeekBackward),
            _ => {}
        }
    }

    match &app.view {
        View::Login(_) => match key.code {
            KeyCode::Tab => Some(Action::LoginFieldNext),
            KeyCode::BackTab => Some(Action::LoginFieldPrev),
            KeyCode::Enter => Some(Action::LoginSubmit),
            KeyCode::Backspace => Some(Action::LoginBackspace),
            KeyCode::Char(ch) => Some(Action::LoginType(ch)),
            KeyCode::Esc => Some(Action::Quit),
            _ => None,
        },
        View::PodcastList(_) => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Down => Some(Action::ListDown),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::ListUp),
            KeyCode::PageDown => Some(Action::PageDown),
            KeyCode::PageUp => Some(Action::PageUp),
            KeyCode::Enter => Some(Action::SelectPodcast),
            KeyCode::Char('r') => Some(Action::RefreshSync),
            _ => None,
        },
        View::EpisodeList(_) => match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Esc => Some(Action::NavigateBack),
            KeyCode::Char('j') | KeyCode::Down => Some(Action::ListDown),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::ListUp),
            KeyCode::PageDown => Some(Action::PageDown),
            KeyCode::PageUp => Some(Action::PageUp),
            KeyCode::Enter => Some(Action::SelectEpisode),
            KeyCode::Char('p') => Some(Action::PlayEpisode),
            KeyCode::Char('d') => Some(Action::ToggleDone),
            KeyCode::Char('r') => Some(Action::RefreshSync),
            _ => None,
        },
        View::EpisodeDetail(_) => match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Esc => Some(Action::NavigateBack),
            KeyCode::Char('j') | KeyCode::Down => Some(Action::ScrollDown),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::ScrollUp),
            KeyCode::PageDown => Some(Action::PageDown),
            KeyCode::PageUp => Some(Action::PageUp),
            KeyCode::Char('p') | KeyCode::Enter => Some(Action::PlayEpisode),
            _ => None,
        },
    }
}

/// Handle actions that require async work (login, sync, playback). These
/// spawn tokio tasks and send results back through the action channel.
pub fn handle_async_action(action: &Action, app: &mut App, player: &PlayerHandle) {
    match action {
        Action::LoginSubmit => {
            if let View::Login(ref mut state) = app.view {
                if state.loading {
                    return;
                }
                state.loading = true;
                state.error = None;
                let server_url = state.server_url.clone();
                let username = state.username.clone();
                let password = state.password.clone();
                let tx = app.action_tx.clone();
                let db_path = app.db.path().to_string();

                tokio::spawn(async move {
                    let client = ApiClient::new(&server_url, None);
                    match client.login(&username, &password).await {
                        Ok((token, _expires_at)) => {
                            let db = LocalDb::open(&db_path)
                                .expect("failed to open local db");
                            db.set_config("server_url", &server_url);
                            db.set_config("auth_token", &token);
                            db.set_config("username", &username);
                            let _ = tx.send(Action::LoginResult(Ok(username)));
                        }
                        Err(e) => {
                            let _ = tx.send(Action::LoginResult(Err(e.to_string())));
                        }
                    }
                });
            }
        }
        Action::PushProgress => {
            let tx = app.action_tx.clone();
            let db_path = app.db.path().to_string();

            tokio::spawn(async move {
                let result = push_dirty_progress(&db_path).await;
                match result {
                    Ok(count) => { let _ = tx.send(Action::PushProgressComplete(Ok(count))); }
                    Err(e) => { let _ = tx.send(Action::PushProgressComplete(Err(e.to_string()))); }
                }
            });
        }
        Action::RefreshSync => {
            let tx = app.action_tx.clone();
            let db_path = app.db.path().to_string();

            tokio::spawn(async move {
                let result = crate::tui::sync::run_sync(&db_path).await;
                match result {
                    Ok(()) => { let _ = tx.send(Action::SyncComplete(Ok(()))); }
                    Err(e) => { let _ = tx.send(Action::SyncComplete(Err(e.to_string()))); }
                }
            });
        }

        // -- Playback actions --

        Action::PlayEpisode => {
            // Determine which episode to play from the current view.
            let episode = match &app.view {
                View::EpisodeList(s) => s.episodes.get(s.selected).cloned(),
                View::EpisodeDetail(s) => Some(s.episode.clone()),
                _ => None,
            };
            let Some(episode) = episode else { return };

            let tx = app.action_tx.clone();
            let player = Arc::clone(player);
            let audio_url = episode.audio_url.clone();
            let start_pos = episode.progress;
            let _episode_id = episode.id.clone();
            let _episode_title = episode.title.clone();

            tokio::spawn(async move {
                // Stop any existing playback first.
                {
                    let mut guard = player.lock().await;
                    if let Some(mut p) = guard.take() {
                        let _ = p.stop().await;
                    }
                }

                match Player::start(&audio_url, start_pos).await {
                    Ok(new_player) => {
                        *player.lock().await = Some(new_player);
                        let _ = tx.send(Action::PlaybackStarted(Ok(())));
                    }
                    Err(e) => {
                        let _ = tx.send(Action::PlaybackStarted(Err(e.to_string())));
                        return;
                    }
                }

                // Send the NowPlaying info. We piggyback this on
                // PlaybackUpdate with initial state.
                let _ = tx.send(Action::PlaybackUpdate(PlaybackState {
                    position_secs: start_pos,
                    duration_secs: 0,
                    paused: false,
                    finished: false,
                }));
            });
            // app.update(PlayEpisode) sets now_playing from the current
            // view state before this async task completes.
        }

        Action::TogglePause => {
            let player = Arc::clone(player);
            tokio::spawn(async move {
                let guard = player.lock().await;
                if let Some(ref p) = *guard {
                    let _ = p.toggle_pause().await;
                }
            });
        }

        Action::SeekForward => {
            let player = Arc::clone(player);
            tokio::spawn(async move {
                let guard = player.lock().await;
                if let Some(ref p) = *guard {
                    let _ = p.seek(30).await;
                }
            });
        }

        Action::SeekBackward => {
            let player = Arc::clone(player);
            tokio::spawn(async move {
                let guard = player.lock().await;
                if let Some(ref p) = *guard {
                    let _ = p.seek(-15).await;
                }
            });
        }

        Action::StopPlayback => {
            let player = Arc::clone(player);
            tokio::spawn(async move {
                let mut guard = player.lock().await;
                if let Some(mut p) = guard.take() {
                    let _ = p.stop().await;
                }
            });
        }

        _ => {}
    }
}

/// Spawn a background task that polls mpv for playback state every second
/// and sends updates to the app.
pub fn spawn_playback_poller(player: PlayerHandle, tx: tokio::sync::mpsc::UnboundedSender<Action>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            let mut guard = player.lock().await;
            if let Some(ref mut p) = *guard {
                let state = p.poll_state().await;
                if state.finished {
                    let _ = tx.send(Action::PlaybackFinished);
                    if let Some(mut p) = guard.take() {
                        let _ = p.stop().await;
                    }
                } else {
                    let _ = tx.send(Action::PlaybackUpdate(state));
                }
            }
        }
    });
}

/// Spawn a background task that pushes dirty progress to the server every
/// 30 seconds.
pub fn spawn_progress_pusher(tx: tokio::sync::mpsc::UnboundedSender<Action>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let _ = tx.send(Action::PushProgress);
        }
    });
}

/// Push all dirty local progress entries to the server.
async fn push_dirty_progress(db_path: &str) -> anyhow::Result<usize> {
    let db = LocalDb::open(db_path).context("open local database")?;

    let server_url = db.get_config("server_url")
        .ok_or_else(|| anyhow::anyhow!("no server_url configured"))?;
    let token = db.get_config("auth_token")
        .ok_or_else(|| anyhow::anyhow!("not logged in"))?;

    let client = ApiClient::new(&server_url, Some(token));
    let dirty = db.list_dirty_progress();
    let mut pushed = 0;

    for (episode_id, progress, done) in &dirty {
        match client.report_progress(episode_id, *progress, *done).await {
            Ok(_) => {
                db.mark_progress_clean(episode_id);
                pushed += 1;
            }
            Err(e) => {
                tracing::warn!(episode_id, "failed to push progress: {}", e);
            }
        }
    }

    Ok(pushed)
}
