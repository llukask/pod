use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use tokio::sync::Mutex;

use pod::tui::app::{Action, App, View};
use pod::tui::event::{self, PlayerHandle};
use pod::tui::local_db::LocalDb;
use pod::tui::mpris;
use pod::tui::ui;

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // Use a LocalSet so we can spawn !Send futures (MPRIS Player uses Rc).
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, run())
}

async fn run() -> Result<()> {
    // Determine the data directory: ~/.local/share/pod/
    let data_dir = dirs::data_dir()
        .expect("could not determine data directory")
        .join("pod");
    std::fs::create_dir_all(&data_dir)?;
    // Restrict the data directory to owner-only access since it contains
    // auth tokens in the SQLite database.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let db_path = data_dir.join("pod.db");

    let db = LocalDb::open(db_path.to_str().expect("invalid db path"))?;
    let mut app = App::new(db);

    // Shared mpv player handle.
    let player: PlayerHandle = Arc::new(Mutex::new(None));

    // Spawn background task that polls mpv for playback progress.
    event::spawn_playback_poller(Arc::clone(&player), app.action_tx.clone());

    // Spawn background task that pushes dirty progress to the server
    // every 30 seconds.
    event::spawn_progress_pusher(app.action_tx.clone());

    // Set up MPRIS D-Bus service for system media keys and GNOME tray.
    let mpris_player: Option<Rc<mpris_server::Player>> =
        match mpris::create_mpris_player(app.action_tx.clone()).await {
            Ok(p) => {
                // The MPRIS event loop must run on the local task set
                // since Player is !Send.
                let p_clone = Rc::clone(&p);
                tokio::task::spawn_local(async move {
                    p_clone.run().await;
                });
                Some(p)
            }
            Err(e) => {
                eprintln!("warning: MPRIS setup failed (media keys won't work): {}", e);
                None
            }
        };

    // If we already have a token, kick off a sync immediately.
    if matches!(app.view, View::Inbox(_) | View::PodcastList(_)) {
        let _ = app.action_tx.send(Action::RefreshSync);
    }

    // Set up terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Main loop.
    use crossterm::event::EventStream;
    use tokio_stream::StreamExt;

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        terminal.draw(|f| ui::render(f, &app))?;

        tokio::select! {
            maybe_event = events.next() => {
                if let Some(Ok(crossterm::event::Event::Key(key))) = maybe_event {
                    if let Some(action) = event::map_key(&app, key) {
                        event::handle_async_action(&action, &mut app, &player);
                        app.update(action);
                    }
                }
            }
            Some(action) = app.action_rx.recv() => {
                event::handle_async_action(&action, &mut app, &player);
                app.update(action);
            }
            _ = tick.tick() => {
                app.update(Action::Tick);
            }
        }

        // Keep MPRIS state in sync with the TUI.
        if let Some(ref mpris_p) = mpris_player {
            if let Some(ref np) = app.now_playing {
                mpris::update_mpris_state(
                    mpris_p,
                    &np.episode_title,
                    np.state.position_secs,
                    np.state.duration_secs,
                    np.state.paused,
                    true,
                )
                .await;
            } else {
                mpris::update_mpris_state(mpris_p, "", 0, 0, false, false).await;
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Stop playback before exiting.
    {
        let mut guard = player.lock().await;
        if let Some(mut p) = guard.take() {
            let _ = p.stop().await;
        }
    }

    // Restore terminal.
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
