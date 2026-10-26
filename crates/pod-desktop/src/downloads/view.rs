//! The Downloads page (wireframe D4) and the download controls the inbox's
//! detail pane shares with it.
//!
//! [`DownloadList`] is one model for both: the live query of every download
//! and wish, plus what only the reconciler knows (the auto wishes the budget
//! skipped, whether the pins alone overflow it, and live byte progress).

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px,
};
use pod_protocol::{EpisodeId, PodcastId, Timestamp};
use tokio::sync::watch;

use super::{
    Act, Entry, Removed, Status, actions, load, perform, size_label, source_label, status,
    status_label, tables, undo,
};
use crate::download::{Progress, WaitReason};
use crate::reconcile::Reconciler;
use crate::reconcile::plan::Fit;
use crate::runtime::Background;
use crate::store::{Store, live};
use crate::sync::SyncHandle;

// ==============================================================================
// The shared model
// ==============================================================================

/// Every download and wish, with the reconciler's in-memory state.
pub struct DownloadList {
    /// `None` until the first query result arrives.
    entries: Option<Vec<Entry>>,
    fit: Fit,
    progress: HashMap<String, Progress>,
    _query: Task<()>,
    _fit: Task<()>,
    _progress: Task<()>,
}

impl std::fmt::Debug for DownloadList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadList")
            .field("entries", &self.entries.as_ref().map(Vec::len))
            .field("fit", &self.fit)
            .finish_non_exhaustive()
    }
}

/// How often live progress re-renders at most. A transfer reports every
/// chunk.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

impl DownloadList {
    /// Requires the [`Store`] and [`Reconciler`] globals.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let reconciler = cx.global::<Reconciler>().clone();
        Self {
            entries: None,
            fit: Fit::default(),
            progress: HashMap::new(),
            _query: live::observe_tables(cx, tables(), load, |this: &mut Self, entries, _| {
                this.entries = Some(entries);
            }),
            _fit: follow(cx, reconciler.fit(), None, |this, fit| this.fit = fit),
            _progress: follow(
                cx,
                reconciler.progress(),
                Some(PROGRESS_EVERY),
                |this, progress| this.progress = progress,
            ),
        }
    }

    pub fn entry(&self, id: EpisodeId) -> Option<&Entry> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.episode_id == id)
    }

    pub fn status(&self, entry: &Entry) -> Status {
        status(entry, &self.fit, &self.progress)
    }
}

/// Hands every value of `values` to `apply`, at most one per `throttle`.
fn follow<T: Clone + Send + Sync + 'static>(
    cx: &mut Context<DownloadList>,
    mut values: watch::Receiver<T>,
    throttle: Option<Duration>,
    apply: fn(&mut DownloadList, T),
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            let value = values.borrow_and_update().clone();
            let applied = this.update(cx, |this, cx| {
                apply(this, value);
                cx.notify();
            });
            if applied.is_err() || values.changed().await.is_err() {
                return;
            }
            if let Some(throttle) = throttle {
                cx.background_executor().timer(throttle).await;
            }
        }
    })
}

// ==============================================================================
// Actions
// ==============================================================================

/// Does `act` in the background, wakes the reconciler, and offers an undo
/// when the episode was archived.
pub fn run(
    act: Act,
    episode_id: EpisodeId,
    podcast_id: PodcastId,
    window: &mut Window,
    cx: &mut App,
) {
    let store = cx.global::<Store>().clone();
    let sync = cx.global::<SyncHandle>().clone();
    let reconciler = cx.global::<Reconciler>().clone();
    window
        .spawn(cx, async move |cx| {
            match perform(&store, &sync, episode_id, podcast_id, act).await {
                Ok(removed) => {
                    reconciler.wake();
                    if let Some(removed) = removed {
                        let shown = cx.update(|window, cx| {
                            window.push_notification(archived_notification(removed), cx);
                        });
                        if shown.is_err() {
                            tracing::warn!("the window closed before the undo showed");
                        }
                    }
                }
                // TODO: tell the user, once the window has a place for errors.
                Err(error) => tracing::error!("{act:?} on episode {episode_id} failed: {error:#}"),
            }
        })
        .detach();
}

/// "Archived and removed" with Undo (spec 7.4).
fn archived_notification(removed: Removed) -> Notification {
    Notification::new()
        .content(|_, _, _| {
            div()
                .id("downloads-archived")
                .aria_label("Archived and removed")
                .test_support()
                .child("Archived and removed")
                .into_any_element()
        })
        .action(move |_, _, cx| {
            let removed = removed.clone();
            Button::new("downloads-undo")
                .label("Undo")
                .small()
                .on_click(cx.listener(move |note, _, window, cx| {
                    let store = cx.global::<Store>().clone();
                    let sync = cx.global::<SyncHandle>().clone();
                    let removed = removed.clone();
                    cx.global::<Background>().spawn(async move {
                        if let Err(error) = undo(&store, &sync, removed).await {
                            tracing::error!("undoing a removal failed: {error:#}");
                        }
                    });
                    note.dismiss(window, cx);
                }))
        })
        // An action turns autohide off; the undo is a convenience, so the
        // toast still goes by itself.
        .autohide(true)
}

/// The buttons for `status`, with ids `<prefix>-<act>-<episode id>`.
pub fn buttons(prefix: &str, entry: &Entry, status: &Status) -> impl IntoElement {
    let (episode_id, podcast_id) = (entry.episode_id, entry.podcast_id);
    h_flex()
        .gap_1()
        .children(actions(status).iter().map(|&act| {
            let button = Button::new(SharedString::from(format!(
                "{prefix}-{}-{episode_id}",
                act.key()
            )))
            .small()
            .label(act.label())
            .on_click(move |_, window, cx| run(act, episode_id, podcast_id, window, cx));
            match act {
                Act::Retry | Act::DownloadAnyway => button.outline(),
                _ => button.ghost(),
            }
        }))
}

// ==============================================================================
// The page
// ==============================================================================

/// The page's request to open Settings, where the budget is.
#[derive(Debug, Clone, Copy)]
pub struct OpenSettings;

/// The Downloads page.
pub struct DownloadsView {
    list: Entity<DownloadList>,
    /// "Manage pins" narrows the table to pinned downloads.
    pins_only: bool,
    _list_changed: Subscription,
    /// Countdowns ("Retrying in 2 min") age without a write.
    _ticker: Task<()>,
}

impl std::fmt::Debug for DownloadsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadsView")
            .field("pins_only", &self.pins_only)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<OpenSettings> for DownloadsView {}

impl DownloadsView {
    pub fn new(list: Entity<DownloadList>, cx: &mut Context<Self>) -> Self {
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        });
        Self {
            _list_changed: cx.observe(&list, |_, _, cx| cx.notify()),
            list,
            pins_only: false,
            _ticker: ticker,
        }
    }
}

impl Render for DownloadsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = SharedString::from("Downloads");
        let root = v_flex()
            .id("content")
            .flex_1()
            .min_w_0()
            .h_full()
            .p_6()
            .gap_3()
            .child(
                div()
                    .id("page-title")
                    .aria_label(title.clone())
                    .test_support()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            );
        let list = self.list.read(cx);
        let Some(entries) = &list.entries else {
            return root;
        };
        let now = Timestamp::now().as_epoch_ms();
        let rows: Vec<(Entry, Status)> = entries
            .iter()
            .filter(|entry| !self.pins_only || entry.pinned)
            .map(|entry| (entry.clone(), list.status(entry)))
            .collect();
        let storage_full = entries
            .iter()
            .any(|entry| list.status(entry) == Status::Queued(Some(WaitReason::Space)));
        let pins_over_budget = list.fit.pins_over_budget;
        let empty = entries.is_empty();

        root.when(storage_full, |root| {
            root.child(self.banner("downloads-storage-full", "Storage full, downloads paused", cx))
        })
        .when(pins_over_budget, |root| {
            root.child(self.banner(
                "downloads-pins-over-budget",
                "Pinned downloads exceed your budget, auto downloads are off",
                cx,
            ))
        })
        .when(self.pins_only, |root| {
            root.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child("Showing pinned downloads")
                    .child(
                        Button::new("downloads-show-all")
                            .ghost()
                            .small()
                            .label("Show all")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pins_only = false;
                                cx.notify();
                            })),
                    ),
            )
        })
        .child(if empty {
            div()
                .id("downloads-empty")
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Nothing downloaded. Episodes you download, and a show's auto downloads, show up here.")
                .into_any_element()
        } else {
            render_table(&rows, now, cx)
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Downloads run while Pod is open and resume where they stopped next time."),
        )
    }
}

impl DownloadsView {
    /// A warning with Change budget and Manage pins (D4).
    fn banner(&self, id: &'static str, text: &'static str, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id(id)
            .aria_label(text)
            .test_support()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(cx.theme().warning.opacity(0.15))
            .child(div().flex_1().min_w_0().text_sm().child(text))
            .child(
                Button::new(SharedString::from(format!("{id}-budget")))
                    .outline()
                    .small()
                    .label("Change budget")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenSettings))),
            )
            .child(
                Button::new(SharedString::from(format!("{id}-pins")))
                    .ghost()
                    .small()
                    .label("Manage pins")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pins_only = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

// TODO: the storage meter above the table (D4 annotation 1), and
// "playing" on the file the player holds.
fn render_table(rows: &[(Entry, Status)], now: i64, cx: &mut Context<DownloadsView>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let header = |text: &'static str| div().text_xs().text_color(muted).child(text);
    v_flex()
        .id("downloads-table")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(
            h_flex()
                .gap_3()
                .py_1()
                .child(header("Episode").flex_1().min_w_0())
                .child(header("Status").w(px(300.)))
                .child(header("Source").w(px(64.)))
                .child(header("Size").w(px(80.)))
                .child(div().w(px(260.))),
        )
        .children(rows.iter().map(|(entry, status)| {
            let id = entry.episode_id;
            let label = SharedString::from(status_label(status, now));
            h_flex()
                .id(SharedString::from(format!("download-row-{id}")))
                .gap_3()
                .py_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().truncate().child(entry.title.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(entry.podcast_title.clone()),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("download-status-{id}")))
                        .aria_label(label.clone())
                        .test_support()
                        .w(px(300.))
                        .text_sm()
                        .child(label),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("download-source-{id}")))
                        .aria_label(source_label(entry))
                        .test_support()
                        .w(px(64.))
                        .text_sm()
                        .child(source_label(entry)),
                )
                .child(div().w(px(80.)).text_sm().child(size_label(entry)))
                .child(
                    h_flex()
                        .w(px(260.))
                        .justify_end()
                        .child(buttons("download", entry, status)),
                )
        }))
        .into_any_element()
}

/// The detail pane's download control for episode `id` (D1 annotation 4):
/// Download when nothing is wished, otherwise the status and the same
/// actions as the Downloads page.
pub fn detail_control(
    list: &Entity<DownloadList>,
    episode_id: EpisodeId,
    podcast_id: PodcastId,
    cx: &App,
) -> AnyElement {
    let list = list.read(cx);
    let Some(entry) = list.entry(episode_id) else {
        return Button::new("detail-download")
            .outline()
            .label(Act::Download.label())
            .on_click(move |_, window, cx| {
                run(Act::Download, episode_id, podcast_id, window, cx);
            })
            .into_any_element();
    };
    let status = list.status(entry);
    let label = SharedString::from(status_label(&status, Timestamp::now().as_epoch_ms()));
    h_flex()
        .gap_2()
        .child(
            div()
                .id("detail-download-status")
                .aria_label(label.clone())
                .test_support()
                .text_sm()
                .child(label),
        )
        .child(buttons("detail-download", entry, &status))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use color_eyre::eyre::{Result, WrapErr as _};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, TestAppContext};
    use rusqlite::{Transaction, params};

    use super::*;
    use crate::app::Route;
    use crate::app::tests::{TestApp, signed_in};
    use crate::shell;

    const PODCAST: &str = "a1000000-0000-5000-8000-000000000001";
    const HOUR_MS: i64 = 3_600_000;

    fn episode(n: u32) -> String {
        format!("e0000000-0000-5000-8000-{n:012}")
    }

    /// One episode per state of the D4 table, newest first:
    ///
    /// 1. downloading, pinned
    /// 2. retrying, auto
    /// 3. held off a metered network, auto
    /// 4. failed, pinned
    /// 5. downloaded, pinned
    /// 6. downloaded and leaving, no wish
    /// 7. paused for storage, pinned
    /// 8. an auto wish without a row
    fn seed(transaction: &Transaction<'_>) -> Result<()> {
        signed_in(transaction)?;
        let now = Timestamp::now().as_epoch_ms();
        transaction
            .execute(
                "insert into podcast (id, feed_url, status, title, version)
                 values (?1, 'https://example.com/feed', 'ok', 'Hard Fork', 1)",
                [PODCAST],
            )
            .context("insert the podcast")?;
        for n in 1..=8 {
            transaction
                .execute(
                    "insert into episode (id, podcast_id, title, summary_html, content_html,
                         published_at, audio_url, audio_type, audio_bytes, version)
                     values (?1, ?2, 'Episode ' || ?3, '', '', ?4,
                             'https://example.com/a.mp3', 'audio/mpeg', 38000000, 1)",
                    params![episode(n), PODCAST, n, now - i64::from(n) * HOUR_MS],
                )
                .context("insert an episode")?;
        }
        for (n, source) in [
            (1, "pin"),
            (2, "auto"),
            (3, "auto"),
            (4, "pin"),
            (5, "pin"),
            (7, "pin"),
            (8, "auto"),
        ] {
            transaction
                .execute(
                    "insert into download_wish (episode_id, source, created_at)
                     values (?1, ?2, 0)",
                    params![episode(n), source],
                )
                .context("insert a wish")?;
        }
        let rows: [(u32, &str, &str); 7] = [
            (
                1,
                "transferring",
                "bytes_done = 31000000, bytes_total = 50000000",
            ),
            (
                2,
                "waiting_retry",
                "attempts = 2, last_error = 'connection reset', next_attempt_at = :now + 90000",
            ),
            (3, "queued", "wait_reason = 'wifi'"),
            (4, "failed", "last_error = 'HTTP 404 Not Found'"),
            (5, "complete", "bytes_done = 48000000"),
            (
                6,
                "complete",
                "bytes_done = 61000000, unwanted_since = :now",
            ),
            (7, "queued", "wait_reason = 'space'"),
        ];
        for (n, state, set) in rows {
            transaction
                .execute(
                    "insert into download (episode_id, state, audio_url, path)
                     values (?1, ?2, 'https://example.com/a.mp3', '')",
                    params![episode(n), state],
                )
                .context("insert a row")?;
            transaction
                .execute(
                    &format!(
                        "update download set {} where episode_id = ?1",
                        set.replace(":now", &now.to_string())
                    ),
                    [episode(n)],
                )
                .context("set the row's columns")?;
        }
        Ok(())
    }

    fn open_downloads(cx: &mut TestAppContext) -> TestApp {
        let app = TestApp::open(cx, seed);
        app.wait_for_label(cx, "page-title", "Inbox");
        cx.update_window(app.window, |_, window, cx| {
            window
                .within("sidebar")
                .click(shell::sidebar::page_element_id(Route::Downloads), cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, &format!("download-status-{}", episode(8)), "Queued");
        app
    }

    fn status_id(n: u32) -> String {
        format!("download-status-{}", episode(n))
    }

    fn buttons_of(window: &mut Window, n: u32) -> Vec<&'static str> {
        [
            Act::Cancel,
            Act::Remove,
            Act::Retry,
            Act::UseMetered,
            Act::DownloadAnyway,
            Act::Keep,
            Act::Archive,
        ]
        .into_iter()
        .filter(|act| {
            window
                .try_find(format!("download-{}-{}", act.key(), episode(n)))
                .is_some()
        })
        .map(Act::label)
        .collect()
    }

    #[gpui_kit::test]
    fn each_state_shows_its_label_source_and_actions(cx: &mut TestAppContext) {
        let app = open_downloads(cx);
        let reconciler = cx.update(|cx| cx.global::<Reconciler>().clone());
        reconciler.show_fit(Fit {
            skipped: BTreeSet::from([episode(8)]),
            pins_over_budget: true,
        });
        app.wait_for_label(cx, &status_id(8), "Skipped · doesn't fit the budget");
        app.wait_for_label(cx, &status_id(1), "Downloading · 31.0 MB of 50.0 MB");

        let expected: [(u32, &str, &str, &[&str]); 8] = [
            (1, "Downloading · 31.0 MB of 50.0 MB", "Pinned", &["Cancel"]),
            (
                2,
                "Retrying in 2 min · connection reset (attempt 3 of 8)",
                "Auto",
                &["Cancel"],
            ),
            (
                3,
                "Waiting · metered network",
                "Auto",
                &["Cancel", "Use metered network"],
            ),
            (
                4,
                "Failed · HTTP 404 Not Found",
                "Pinned",
                &["Remove", "Retry"],
            ),
            (5, "Downloaded", "Pinned", &["Remove"]),
            (6, "Downloaded · removed in 24 h", "–", &["Remove", "Keep"]),
            (7, "Paused · storage full", "Pinned", &["Cancel"]),
            (
                8,
                "Skipped · doesn't fit the budget",
                "Auto",
                &["Download anyway", "Archive"],
            ),
        ];
        cx.update_window(app.window, |_, window, cx| {
            window.render_frame(cx);
            for (n, label, source, buttons) in expected {
                assert_eq!(window.find(status_id(n)).label(), Some(label), "{n}");
                assert_eq!(
                    window
                        .find(format!("download-source-{}", episode(n)))
                        .label(),
                    Some(source),
                    "{n}"
                );
                assert_eq!(buttons_of(window, n), buttons, "{n}");
            }
            assert!(window.find("downloads-storage-full").visible());
            assert!(window.find("downloads-pins-over-budget").visible());

            window.click("downloads-pins-over-budget-pins", cx);
            assert!(window.try_find(status_id(2)).is_none(), "auto rows hide");
            assert!(window.try_find(status_id(1)).is_some(), "pins stay");
            window.click("downloads-show-all", cx);
            assert!(window.try_find(status_id(2)).is_some());

            window.click("downloads-storage-full-budget", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "page-title", "Settings");
    }

    /// `(pinned, metered_ok)` of episode `n`'s pin, if it has one.
    fn pin(app: &TestApp, n: u32) -> Option<bool> {
        app.read(move |connection| {
            use rusqlite::OptionalExtension as _;
            connection
                .query_row(
                    "select metered_ok from download_wish
                     where episode_id = ?1 and source = 'pin'",
                    [episode(n)],
                    |row| row.get(0),
                )
                .optional()
                .context("read the pin")
        })
    }

    fn archived(app: &TestApp, n: u32) -> bool {
        app.read(move |connection| {
            use rusqlite::OptionalExtension as _;
            Ok(connection
                .query_row(
                    "select archived from episode_state where episode_id = ?1",
                    [episode(n)],
                    |row| row.get(0),
                )
                .optional()
                .context("read the archived flag")?
                .unwrap_or(false))
        })
    }

    #[gpui_kit::test]
    fn actions_write_wishes_and_cancelling_an_auto_row_archives_with_undo(cx: &mut TestAppContext) {
        let app = open_downloads(cx);
        cx.update_window(app.window, |_, window, cx| {
            window.click(format!("download-metered-{}", episode(3)), cx);
            window.click(format!("download-retry-{}", episode(4)), cx);
            window.click(format!("download-keep-{}", episode(6)), cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, &format!("download-source-{}", episode(3)), "Pinned");
        app.wait_for_label(cx, &format!("download-source-{}", episode(6)), "Pinned");
        app.wait_for_label(cx, &status_id(4), "Queued");
        assert_eq!(
            pin(&app, 3),
            Some(true),
            "the pin may use a metered network"
        );
        assert_eq!(pin(&app, 6), Some(false));
        assert!(!archived(&app, 3), "a pin keeps the episode in the inbox");

        cx.update_window(app.window, |_, window, cx| {
            window.click(format!("download-cancel-{}", episode(2)), cx);
        })
        .expect("the window is still open");
        // The reconciler, not running here, deletes the row itself.
        app.wait_for_label(cx, "downloads-archived", "Archived and removed");
        app.wait_for_label(cx, &format!("download-source-{}", episode(2)), "–");
        assert!(archived(&app, 2));

        cx.update_window(app.window, |_, window, cx| {
            window.click("downloads-undo", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, &format!("download-source-{}", episode(2)), "Auto");
        assert!(!archived(&app, 2));

        cx.update_window(app.window, |_, window, cx| {
            window.click(format!("download-cancel-{}", episode(1)), cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, &format!("download-source-{}", episode(1)), "–");
        assert!(!archived(&app, 1), "cancelling a pin doesn't archive");
    }
}
