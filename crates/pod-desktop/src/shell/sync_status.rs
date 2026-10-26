//! The sync status of spec 5.6: its wording, the outbox and rejected-log
//! counts behind it, and the manual refresh (refresh button, `Ctrl+R`). The
//! title bar shows the status; Settings › Sync repeats it with the counts.

use std::time::Instant;

use color_eyre::eyre::{Result, WrapErr as _};
use gpui_kit::{App, KeyBinding};
use pod_protocol::Timestamp;
use rusqlite::Connection;

use crate::runtime::Background;
use crate::store::{Table, TableSet};
use crate::sync::engine::{EngineState, SyncEngine};

gpui_kit::actions!(sync, [SyncNow]);

/// Binds `Ctrl+R` everywhere in the window.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-r", SyncNow, None)]);
}

/// Runs a sync loop now. It goes to the engine directly, not through the
/// driver's write deadlines, so it also cuts a backoff short.
pub fn sync_now(cx: &mut App) {
    let engine = cx.global::<SyncEngine>().clone();
    cx.global::<Background>().spawn(async move {
        if let Err(error) = engine.sync().await {
            tracing::error!(error = format!("{error:#}"), "manual sync failed locally");
        }
    });
}

// ==============================================================================
// Counts
// ==============================================================================

/// The local queue sizes the status reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncCounts {
    /// Outbox rows: changes not yet on the server.
    pub waiting: i64,
    /// Rejected-log rows: changes the server refused or that were dropped
    /// after 5 attempts.
    pub rejected: i64,
}

pub fn tables() -> TableSet {
    [Table::Outbox, Table::RejectedMutation]
        .into_iter()
        .collect()
}

pub fn load_counts(connection: &Connection) -> Result<SyncCounts> {
    connection
        .query_row(
            "select (select count(*) from outbox), (select count(*) from rejected_mutation)",
            [],
            |row| {
                Ok(SyncCounts {
                    waiting: row.get(0)?,
                    rejected: row.get(1)?,
                })
            },
        )
        .context("count the outbox and the rejected log")
}

// ==============================================================================
// Wording
// ==============================================================================

/// The status in the words of spec 5.6. `now` and `wall_now` are the same
/// moment on the two clocks the engine state uses.
pub fn status_label(
    state: &EngineState,
    waiting: i64,
    now: Instant,
    wall_now: Timestamp,
) -> String {
    match state {
        EngineState::Idle {
            last_synced: Some(at),
        } => {
            let minutes = (wall_now.as_epoch_ms() - at.as_epoch_ms()).max(0) / 60_000;
            match minutes {
                0 => "Synced just now".to_owned(),
                1..60 => format!("Synced {minutes} min ago"),
                _ => format!("Synced {} h ago", minutes / 60),
            }
        }
        // Before the first loop of this process ends, or after the local
        // store failed one.
        EngineState::Idle { last_synced: None } => "Not synced yet".to_owned(),
        EngineState::Running { backfill: None } => "Syncing…".to_owned(),
        EngineState::Running {
            backfill: Some(show),
        } => format!("Syncing… Loading episodes for {show}"),
        EngineState::Offline if waiting == 0 => "Offline".to_owned(),
        EngineState::Offline => format!("Offline · {waiting} waiting"),
        EngineState::BackingOff { retry_at } => {
            // Rounded up, so the countdown never reads "0 s" before the
            // retry starts.
            let seconds = retry_at
                .saturating_duration_since(now)
                .as_millis()
                .div_ceil(1000)
                .max(1);
            let in_time = if seconds < 60 {
                format!("{seconds} s")
            } else {
                format!("{} min", seconds.div_ceil(60))
            };
            format!("Can't reach server · retrying in {in_time}")
        }
        EngineState::SignedOut => "Signed out · Sign in".to_owned(),
    }
}

/// The counts line under the status in Settings › Sync, `None` when both
/// are zero.
pub fn counts_label(counts: SyncCounts) -> Option<String> {
    let changes = |count: i64| match count {
        1 => "1 change".to_owned(),
        count => format!("{count} changes"),
    };
    let parts: Vec<String> = [
        (counts.waiting > 0).then(|| format!("{} waiting", changes(counts.waiting))),
        (counts.rejected > 0).then(|| format!("{} couldn't be saved", changes(counts.rejected))),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, TestAppContext};
    use rusqlite::Transaction;

    use super::*;
    use crate::app::tests::{TestApp, signed_in};

    #[test]
    fn each_engine_state_has_its_wording() {
        let now = Instant::now();
        let wall_now = Timestamp::from_epoch_ms(1_800_000_000_000).expect("in range");
        let minutes_ago = |minutes: i64| {
            Timestamp::from_epoch_ms(wall_now.as_epoch_ms() - minutes * 60_000 - 5_000)
                .expect("in range")
        };
        let label = |state: EngineState, waiting| status_label(&state, waiting, now, wall_now);
        let idle = |last_synced| EngineState::Idle { last_synced };
        let backing_off = |delay| EngineState::BackingOff {
            retry_at: now + delay,
        };

        assert_eq!(label(idle(Some(minutes_ago(0))), 0), "Synced just now");
        assert_eq!(label(idle(Some(minutes_ago(2))), 0), "Synced 2 min ago");
        assert_eq!(label(idle(Some(minutes_ago(59))), 0), "Synced 59 min ago");
        assert_eq!(label(idle(Some(minutes_ago(185))), 0), "Synced 3 h ago");
        assert_eq!(label(idle(None), 0), "Not synced yet");
        assert_eq!(
            label(EngineState::Running { backfill: None }, 2),
            "Syncing…"
        );
        assert_eq!(
            label(
                EngineState::Running {
                    backfill: Some("Syntax".into())
                },
                0
            ),
            "Syncing… Loading episodes for Syntax"
        );
        assert_eq!(label(EngineState::Offline, 3), "Offline · 3 waiting");
        assert_eq!(label(EngineState::Offline, 0), "Offline");
        assert_eq!(
            label(backing_off(Duration::from_secs(4 * 60)), 1),
            "Can't reach server · retrying in 4 min"
        );
        assert_eq!(
            label(backing_off(Duration::from_secs(150)), 1),
            "Can't reach server · retrying in 3 min"
        );
        assert_eq!(
            label(backing_off(Duration::from_millis(4_200)), 1),
            "Can't reach server · retrying in 5 s"
        );
        assert_eq!(
            label(backing_off(Duration::ZERO), 1),
            "Can't reach server · retrying in 1 s"
        );
        assert_eq!(label(EngineState::SignedOut, 4), "Signed out · Sign in");
    }

    #[test]
    fn the_counts_line_names_what_is_queued_and_refused() {
        let counts = |waiting, rejected| counts_label(SyncCounts { waiting, rejected });
        assert_eq!(counts(0, 0), None);
        assert_eq!(counts(2, 0).as_deref(), Some("2 changes waiting"));
        assert_eq!(
            counts(1, 1).as_deref(),
            Some("1 change waiting · 1 change couldn't be saved")
        );
        assert_eq!(counts(0, 3).as_deref(), Some("3 changes couldn't be saved"));
    }

    fn three_waiting(transaction: &Transaction<'_>) -> Result<()> {
        signed_in(transaction)?;
        transaction
            .execute_batch(
                "insert into outbox (mutation_id, coalesce_key, payload, created_at)
                 values ('m1', 'k1', '{}', 0), ('m2', 'k2', '{}', 0), ('m3', 'k3', '{}', 0);
                 insert into rejected_mutation (mutation_id, payload, reason, rejected_at)
                 values ('m0', '{}', 'bad_request', 0);",
            )
            .context("queue three changes and log a refused one")
    }

    fn wait_for_status(app: &TestApp, cx: &mut TestAppContext, label: &str) {
        app.wait_until(cx, &format!("the status reads {label:?}"), |window| {
            window
                .try_find("sync-status")
                .is_some_and(|status| status.label() == Some(label))
        });
    }

    #[gpui_kit::test]
    fn the_title_bar_shows_each_engine_state(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, three_waiting);
        let two_minutes_ago =
            Timestamp::from_epoch_ms(Timestamp::now().as_epoch_ms() - 125_000).expect("in range");
        for (state, label) in [
            (
                EngineState::Idle {
                    last_synced: Some(two_minutes_ago),
                },
                "Synced 2 min ago",
            ),
            (
                EngineState::Running {
                    backfill: Some("Syntax".into()),
                },
                "Syncing… Loading episodes for Syntax",
            ),
            (EngineState::Offline, "Offline · 3 waiting"),
            (
                EngineState::BackingOff {
                    retry_at: Instant::now() + Duration::from_secs(4 * 60),
                },
                "Can't reach server · retrying in 4 min",
            ),
            (EngineState::SignedOut, "Signed out · Sign in"),
        ] {
            app.engine.show_state(state);
            wait_for_status(&app, cx, label);
        }
    }

    #[gpui_kit::test]
    fn clicking_the_status_opens_settings_sync(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, three_waiting);
        app.engine.show_state(EngineState::Offline);
        wait_for_status(&app, cx, "Offline · 3 waiting");
        cx.update_window(app.window, |_, window, cx| {
            window.click("sync-status", cx);
            assert_eq!(window.find("page-title").label(), Some("Settings"));
            assert_eq!(
                window.find("settings-sync-status").label(),
                Some("Offline · 3 waiting")
            );
            assert_eq!(
                window.find("settings-sync-counts").label(),
                Some("3 changes waiting · 1 change couldn't be saved")
            );
        })
        .expect("the window is still open");
    }

    /// An account on a port nothing listens on, so a sync fails at once.
    fn unreachable_server(transaction: &Transaction<'_>) -> Result<()> {
        signed_in(transaction)?;
        transaction
            .execute("update account set server_url = 'http://127.0.0.1:9'", [])
            .context("point the account at a closed port")?;
        Ok(())
    }

    #[gpui_kit::test]
    fn ctrl_r_syncs_now(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, unreachable_server);
        wait_for_status(&app, cx, "Not synced yet");
        cx.update_window(app.window, |_, window, cx| window.press("ctrl-r", cx))
            .expect("the window is still open");
        app.wait_until(cx, "the sync fails and backs off", |_| {
            matches!(*app.engine.state().borrow(), EngineState::BackingOff { .. })
        });
    }
}
