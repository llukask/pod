//! MPRIS D-Bus integration for system media key support and GNOME media tray.
//!
//! Registers a `org.mpris.MediaPlayer2.pod` service on the session bus.
//! GNOME (and other DEs) will show the player in the system tray and route
//! hardware media keys to it.

use std::rc::Rc;

use mpris_server::{Metadata, PlaybackStatus, Player, Time};
use tokio::sync::mpsc;

use crate::tui::app::Action;

/// Create the MPRIS player and wire its callbacks to send Actions back to
/// the TUI main loop. Returns the Player handle (for updating metadata and
/// playback state) and a future that must be spawned with `spawn_local`.
pub async fn create_mpris_player(
    tx: mpsc::UnboundedSender<Action>,
) -> anyhow::Result<Rc<Player>> {
    let player = Player::builder("pod")
        .can_play(true)
        .can_pause(true)
        .can_seek(true)
        .can_control(true)
        .identity("Pod")
        .build()
        .await
        .map_err(|e| anyhow::anyhow!("failed to create MPRIS player: {}", e))?;

    let player = Rc::new(player);

    // Wire MPRIS method calls to our Action channel.
    {
        let tx = tx.clone();
        player.connect_play_pause(move |_| {
            let _ = tx.send(Action::TogglePause);
        });
    }
    {
        let tx = tx.clone();
        player.connect_play(move |_| {
            let _ = tx.send(Action::TogglePause);
        });
    }
    {
        let tx = tx.clone();
        player.connect_pause(move |_| {
            let _ = tx.send(Action::TogglePause);
        });
    }
    {
        let tx = tx.clone();
        player.connect_stop(move |_| {
            let _ = tx.send(Action::StopPlayback);
        });
    }
    {
        let tx = tx.clone();
        player.connect_seek(move |_, offset| {
            let secs = offset.as_secs();
            if secs >= 0 {
                let _ = tx.send(Action::SeekForward);
            } else {
                let _ = tx.send(Action::SeekBackward);
            }
        });
    }

    Ok(player)
}

/// Update the MPRIS player state to match the current TUI playback state.
/// Call this whenever playback state changes.
pub async fn update_mpris_state(
    player: &Player,
    episode_title: &str,
    position_secs: i32,
    duration_secs: i32,
    paused: bool,
    playing: bool,
) {
    let status = if !playing {
        PlaybackStatus::Stopped
    } else if paused {
        PlaybackStatus::Paused
    } else {
        PlaybackStatus::Playing
    };

    let _ = player.set_playback_status(status).await;
    player.set_position(Time::from_micros(position_secs as i64 * 1_000_000));

    let metadata = Metadata::builder()
        .title(episode_title)
        .length(Time::from_micros(duration_secs as i64 * 1_000_000))
        .build();
    let _ = player.set_metadata(metadata).await;
}
