//! The Settings page as wireframe D5 lays it out: Account, Sync, Playback
//! (synced) and Downloads (this device).
//!
//! [`SettingsView`] holds the page's live query and its writes; [`render`]
//! draws the page inside the root view, which owns the account and the
//! sync status the title bar shows too.

use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    Task, TestSupportExt as _, Window, div, px,
};
use jiff::tz::TimeZone;
use pod_protocol::Timestamp;

use super::{Field, SettingsPage, Target, last_sync_label, load, save, step, tables};
use crate::account;
use crate::app::PodApp;
use crate::reconcile::plan::DOWNLOAD_ON_METERED;
use crate::shell::icon::PodIcon;
use crate::shell::layout::GUTTER;
use crate::shell::sync_status::{self, SyncCounts};
use crate::shell::ui::{ROUND, icon, icon_button, kbd, page_title, pill, tabular};
use crate::store::{Store, live};
use crate::sync::SyncHandle;
use crate::sync::session::Account;
use crate::theme::{palette, size};

/// The page's data: a live query of the settings and the rejected log,
/// and the writes, which show at once and reach the store in the
/// background.
pub struct SettingsView {
    /// `None` until the first query result arrives.
    page: Option<SettingsPage>,
    scroll: ScrollHandle,
    /// Scroll Downloads into view once it renders.
    reveal_downloads: bool,
    _query: Task<()>,
}

impl std::fmt::Debug for SettingsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsView")
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

impl SettingsView {
    /// Requires the [`Store`] and [`SyncHandle`] globals.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            page: None,
            scroll: ScrollHandle::new(),
            reveal_downloads: false,
            _query: live::observe_tables(cx, tables(), load, |this: &mut Self, page, _| {
                this.page = Some(page);
            }),
        }
    }

    /// Scrolls Downloads into view on the next render that has it, for
    /// the Downloads page's "Change budget".
    pub fn reveal_downloads(&mut self) {
        self.reveal_downloads = true;
    }

    /// Moves `field` one step up or down.
    fn step(&mut self, field: Field, up: bool, cx: &mut Context<Self>) {
        let Some(page) = &mut self.page else {
            return;
        };
        let current = field.get(&page.values);
        let value = step(&field.steps(), current, up);
        if value == current {
            return;
        }
        // Shown before the write lands, so two quick clicks both count.
        field.set(&mut page.values, value);
        self.save(field.target(), field.json(value), cx);
    }

    fn set_on_metered(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(page) = &mut self.page else {
            return;
        };
        page.values.device.on_metered = on;
        self.save(Target::Device(DOWNLOAD_ON_METERED), on.into(), cx);
    }

    fn save(&self, target: Target, value: serde_json::Value, cx: &mut Context<Self>) {
        cx.notify();
        let store = cx.global::<Store>().clone();
        let sync = cx.global::<SyncHandle>().clone();
        cx.spawn(async move |_, _| {
            if let Err(error) = save(&store, &sync, target, value).await {
                // TODO: tell the user, once the window has a place for errors.
                tracing::error!(?target, "saving a setting failed: {error:#}");
            }
        })
        .detach();
    }
}

/// What Settings › Sync shows from the root view's sync state.
#[derive(Clone, Debug)]
pub struct SyncSummary {
    /// The title bar's status label.
    pub status: SharedString,
    pub counts: SyncCounts,
    /// When a sync loop last finished in this process.
    // TODO: keep it in the store, so it survives a restart.
    pub last_synced: Option<Timestamp>,
}

// ==============================================================================
// Rendering
// ==============================================================================

/// The index of Downloads among the page's children: after the title,
/// Account, Sync and Playback.
const DOWNLOADS_SECTION: usize = 4;

pub fn render(
    view: &Entity<SettingsView>,
    account: &Account,
    sync: SyncSummary,
    cx: &mut Context<PodApp>,
) -> AnyElement {
    let (page, scroll) = view.update(cx, |view, _| {
        if view.reveal_downloads && view.page.is_some() {
            view.reveal_downloads = false;
            view.scroll.scroll_to_item(DOWNLOADS_SECTION);
        }
        (view.page.clone(), view.scroll.clone())
    });
    let host = account
        .server_url
        .split_once("://")
        .map_or(account.server_url.as_str(), |(_, host)| host)
        .trim_end_matches('/')
        .to_owned();
    v_flex()
        .id("content")
        .test_support()
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .px(GUTTER)
        .pt(px(32.))
        .pb(px(48.))
        .gap(px(32.))
        .child(page_title("Settings"))
        .child(
            section("Account", None, cx).child(
                panel(cx).child(row(
                    account.username.clone().into(),
                    Some(format!("Signed in on {host}")),
                    pill("sign-out", cx)
                        .label("Sign out")
                        .on_click(|_, window, cx| account::confirm_logout(window, cx))
                        .into_any_element(),
                    true,
                    cx,
                )),
            ),
        )
        .child(render_sync(sync, page.as_ref(), cx))
        .children(
            page.map(|page| {
                let values = page.values;
                let stepper = |field, label, hint: Option<String>, first, cx: &App| {
                    stepper_row(view, field, field.get(&values), label, hint, first, cx)
                };
                let threshold = Field::PlayedThreshold.label(Field::PlayedThreshold.get(&values));
                let view = view.downgrade();
                let on_metered = move |checked: &bool, _: &mut Window, cx: &mut App| {
                    let _ = view.update(cx, |this, cx| this.set_on_metered(*checked, cx));
                };
                [
                    section(
                        "Playback",
                        Some("These follow your account to every device."),
                        cx,
                    )
                    .child(
                        panel(cx)
                            .child(stepper(Field::Speed, "Speed", None, true, cx))
                            .child(stepper(Field::SkipBack, "Skip back", None, false, cx))
                            .child(stepper(Field::SkipForward, "Skip forward", None, false, cx))
                            .child(stepper(
                                Field::PlayedThreshold,
                                "Mark played near the end",
                                Some(format!("Last {threshold}, or last 2 % if that's shorter")),
                                false,
                                cx,
                            ))
                            .child(stepper(
                                Field::InboxWindow,
                                "Inbox shows the last",
                                None,
                                false,
                                cx,
                            )),
                    )
                    .into_any_element(),
                    section("Downloads", Some("On this computer only."), cx)
                        .child(
                            panel(cx)
                                .child(stepper(Field::Budget, "Storage budget", None, true, cx))
                                .child(row(
                                    "Download on metered networks".into(),
                                    Some("All downloads, including ones you start".to_owned()),
                                    Switch::new("setting-on-metered")
                                        .checked(values.device.on_metered)
                                        .on_click(on_metered)
                                        .into_any_element(),
                                    false,
                                    cx,
                                ))
                                .child(stepper(
                                    Field::EvictGrace,
                                    "Keep removed downloads for up to",
                                    Some(
                                        "Played, archived or unpinned. Gives way when space is \
                                         needed."
                                            .to_owned(),
                                    ),
                                    false,
                                    cx,
                                )),
                        )
                        .into_any_element(),
                ]
            })
            .into_iter()
            .flatten(),
        )
        .child(section("Keyboard", None, cx).child(panel(cx).children(
            KEYS.iter().enumerate().map(|(index, (keys, what))| {
                row(
                    (*what).into(),
                    None,
                    h_flex()
                        .gap(px(4.))
                        .children(keys.iter().map(|key| kbd(key, cx)))
                        .into_any_element(),
                    index == 0,
                    cx,
                )
            }),
        )))
        .into_any_element()
}

/// The window's keys, as the Keyboard section lists them.
const KEYS: [(&[&str], &str); 6] = [
    (&["Enter"], "Play the selected episode"),
    (&["J", "K"], "Next or previous episode"),
    (&["P"], "Mark played"),
    (&["E"], "Archive"),
    (&["Ctrl K"], "Search or add a podcast"),
    (&["Ctrl R"], "Sync now"),
];

/// Settings › Sync, where the title bar's status leads: the status again,
/// the outbox and rejected-log counts and the last full sync (spec 5.6), a
/// sync button, and each change that couldn't be saved with its reason.
// TODO: per-show backfill progress and the last error in full (D5 note 1).
fn render_sync(sync: SyncSummary, page: Option<&SettingsPage>, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let labelled = |id: &'static str, text: String| {
        let text = SharedString::from(text);
        div()
            .id(id)
            .aria_label(text.clone())
            .test_support()
            .text_size(size::SMALL)
            .text_color(p.muted)
            .child(text)
    };
    let counts = sync_status::counts_label(sync.counts);
    let last_synced = sync
        .last_synced
        .map(|at| last_sync_label(at, Timestamp::now(), &TimeZone::system()));
    let rejected = page.map_or(&[][..], |page| &page.rejected[..]);
    // A dot beside the status: the accent once everything is on the
    // server, amber while changes wait.
    let dot = if sync.counts.waiting > 0 || sync.counts.rejected > 0 {
        p.amber
    } else {
        p.accent
    };
    section("Sync", None, cx).child(
        panel(cx)
            .child(
                h_flex()
                    .min_h(px(56.))
                    .px(px(16.))
                    .py(px(10.))
                    .gap(px(16.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(div().flex_none().size(px(8.)).rounded(ROUND).bg(dot))
                                    .child(
                                        div()
                                            .id("settings-sync-status")
                                            .aria_label(sync.status.clone())
                                            .test_support()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(sync.status),
                                    ),
                            )
                            .children(counts.map(|counts| labelled("settings-sync-counts", counts)))
                            .children(last_synced.map(|text| labelled("settings-last-sync", text))),
                    )
                    .child(
                        pill("settings-sync-now", cx)
                            .icon(icon(PodIcon::Refresh, px(16.)))
                            .label("Sync now")
                            .on_click(|_, _, cx| sync_status::sync_now(cx)),
                    ),
            )
            .children(rejected.iter().map(|rejection| {
                let label =
                    SharedString::from(format!("{}: {}", rejection.change, rejection.reason));
                v_flex()
                    .id(SharedString::from(format!(
                        "rejected-{}",
                        rejection.mutation_id
                    )))
                    .aria_label(label)
                    .test_support()
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(p.line)
                    .child(div().child(rejection.change.clone()))
                    .child(
                        div()
                            .text_size(size::SMALL)
                            .text_color(p.danger)
                            .child(rejection.reason.clone()),
                    )
            })),
    )
}

/// A titled group, with a line on where its settings apply.
fn section(title: &'static str, note: Option<&'static str>, cx: &App) -> gpui_kit::Div {
    v_flex()
        .w_full()
        .max_w(px(640.))
        .child(
            div()
                .mb(px(10.))
                .text_size(size::LARGE)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .children(note.map(|note| {
            div()
                .mt(px(-6.))
                .mb(px(10.))
                .text_size(size::SMALL)
                .text_color(palette(cx).muted)
                .child(note)
        }))
}

/// The surface a section's rows sit on.
fn panel(cx: &App) -> gpui_kit::Div {
    let p = palette(cx);
    v_flex()
        .bg(p.surface)
        .border_1()
        .border_color(p.line)
        .rounded(px(12.))
}

/// A setting's name and hint on the left, its control on the right, below
/// a hairline unless it is the panel's `first` row.
fn row(
    label: SharedString,
    hint: Option<String>,
    control: AnyElement,
    first: bool,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    h_flex()
        .min_h(px(56.))
        .px(px(16.))
        .py(px(10.))
        .gap(px(16.))
        .when(!first, |this| this.border_t_1().border_color(p.line))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                .children(
                    hint.map(|hint| div().text_size(size::SMALL).text_color(p.muted).child(hint)),
                ),
        )
        .child(control)
        .into_any_element()
}

/// `field` as a minus button, its value and a plus button in one pill. The
/// buttons grey out at the ends of the field's steps.
fn stepper_row(
    view: &Entity<SettingsView>,
    field: Field,
    value: f64,
    label: &'static str,
    hint: Option<String>,
    first: bool,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let steps = field.steps();
    let click = |up: bool| {
        let view = view.downgrade();
        move |_: &_, _: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |this, cx| this.step(field, up, cx));
        }
    };
    let text = SharedString::from(field.label(value));
    let control = h_flex()
        .flex_none()
        .h(px(36.))
        .p(px(2.))
        .rounded(ROUND)
        .border_1()
        .border_color(p.line)
        .child(
            icon_button(
                SharedString::from(format!("{}-less", field.id())),
                PodIcon::Minus,
                cx,
            )
            .size(px(30.))
            .disabled(step(&steps, value, false) == value)
            .on_click(click(false)),
        )
        .child(
            div()
                .id(field.id())
                .aria_label(text.clone())
                .test_support()
                .w(px(72.))
                .text_center()
                .font_weight(FontWeight::MEDIUM)
                .font_features(tabular())
                .child(text),
        )
        .child(
            icon_button(
                SharedString::from(format!("{}-more", field.id())),
                PodIcon::Plus,
                cx,
            )
            .size(px(30.))
            .disabled(step(&steps, value, true) == value)
            .on_click(click(true)),
        )
        .into_any_element();
    row(label.into(), hint, control, first, cx)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use color_eyre::eyre::{Result, WrapErr as _};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, ScrollDelta, TestAppContext, point, px};
    use rusqlite::Transaction;

    use crate::app::Route;
    use crate::app::tests::{TestApp, signed_in};
    use crate::shell::sidebar::page_element_id;
    use crate::sync::engine::EngineState;

    const EPISODE: &str = "019a3f10-8c2e-7a41-9b3d-000000000001";

    fn open_settings(app: &TestApp, cx: &mut TestAppContext) {
        app.wait_for_label(cx, "page-title", "Inbox");
        cx.update_window(app.window, |_, window, cx| {
            window.click(page_element_id(Route::Settings), cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "page-title", "Settings");
    }

    /// Scrolls the page so all of Playback is above the fold of the test
    /// window.
    fn scroll_to_playback(app: &TestApp, cx: &mut TestAppContext) {
        cx.update_window(app.window, |_, window, cx| {
            window.scroll("content", ScrollDelta::Pixels(point(px(0.), px(-200.))), cx);
            window.render_frame(cx);
        })
        .expect("the window is still open");
    }

    /// Polls the store until `sql` reads `expected`.
    fn wait_for_value(app: &TestApp, sql: &'static str, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let value: Option<String> = app.read(move |connection| {
                use rusqlite::OptionalExtension as _;
                connection
                    .query_row(sql, [], |row| row.get(0))
                    .optional()
                    .context("read the value")
            });
            if value.as_deref() == Some(expected) {
                return;
            }
            assert!(Instant::now() < deadline, "{sql} reads {value:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn two_rejected(transaction: &Transaction<'_>) -> Result<()> {
        signed_in(transaction)?;
        transaction
            .execute(
                "insert into episode (id, podcast_id, title, summary_html, content_html,
                     published_at, audio_url, audio_type, version)
                 values (?1, '019a3f10-8c2e-7a41-9b3d-000000000002', 'Episode 1', '', '',
                     0, 'https://example.com/1.mp3', 'audio/mpeg', 1)",
                [EPISODE],
            )
            .context("insert the episode")?;
        let played = serde_json::json!({
            "type": "episode.played", "id": "019a3f10-8c2e-7a41-9b3d-0000000000a1",
            "at": "2026-10-24T10:12:30.000Z", "dev": "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d",
            "episode_id": EPISODE, "played": true,
        });
        let speed = serde_json::json!({
            "type": "setting.set", "id": "019a3f10-8c2e-7a41-9b3d-0000000000a2",
            "at": "2026-10-24T10:12:30.000Z", "dev": "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d",
            "key": "playback_speed", "value": 9,
        });
        for (id, payload, reason) in [
            ("a1", played, "unknown_episode"),
            ("a2", speed, "quota_exceeded"),
        ] {
            transaction
                .execute(
                    "insert into rejected_mutation (mutation_id, payload, reason, rejected_at)
                     values (?1, ?2, ?3, 0)",
                    (id, payload.to_string(), reason),
                )
                .context("log a rejected change")?;
        }
        Ok(())
    }

    #[gpui_kit::test]
    fn sync_lists_each_change_that_couldnt_be_saved_with_its_reason(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, two_rejected);
        open_settings(&app, cx);
        app.wait_for_label(cx, "settings-sync-counts", "2 changes couldn't be saved");
        app.wait_for_label(
            cx,
            "rejected-a1",
            "Mark “Episode 1” played: The server doesn't know this episode.",
        );
        app.wait_for_label(
            cx,
            "rejected-a2",
            "Change the playback speed: The server refused it (quota_exceeded).",
        );
    }

    /// Enough refused changes to push Downloads below the fold.
    fn many_rejected(transaction: &Transaction<'_>) -> Result<()> {
        signed_in(transaction)?;
        for n in 0..12 {
            transaction
                .execute(
                    "insert into rejected_mutation (mutation_id, payload, reason, rejected_at)
                     values (?1, '{}', 'invalid_mutation', ?2)",
                    (format!("m{n}"), n),
                )
                .context("log a rejected change")?;
        }
        Ok(())
    }

    #[gpui_kit::test]
    fn change_budget_opens_settings_at_the_budget(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, many_rejected);
        app.wait_for_label(cx, "page-title", "Inbox");
        cx.update(|cx| {
            cx.global::<crate::reconcile::Reconciler>()
                .show_fit(crate::reconcile::plan::Fit {
                    skipped: Default::default(),
                    pins_over_budget: true,
                })
        });
        cx.update_window(app.window, |_, window, cx| {
            window
                .within("sidebar")
                .click(page_element_id(Route::Downloads), cx);
        })
        .expect("the window is still open");
        app.wait_until(cx, "the budget banner shows", |window| {
            window
                .try_find("downloads-pins-over-budget-budget")
                .is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            window.click("downloads-pins-over-budget-budget", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "page-title", "Settings");
        app.wait_until(cx, "Settings scrolls to the budget", |window| {
            window
                .try_find("setting-budget-more")
                .is_some_and(|button| button.visible())
        });
    }

    #[gpui_kit::test]
    fn sync_keeps_the_last_full_sync_while_the_next_one_runs(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in);
        open_settings(&app, cx);
        let at = pod_protocol::Timestamp::now();
        app.engine.show_state(EngineState::Idle {
            last_synced: Some(at),
        });
        let label = super::last_sync_label(
            at,
            pod_protocol::Timestamp::now(),
            &jiff::tz::TimeZone::system(),
        );
        app.wait_for_label(cx, "settings-last-sync", &label);
        app.engine
            .show_state(EngineState::Running { backfill: None });
        app.wait_for_label(cx, "settings-sync-status", "Syncing…");
        app.wait_for_label(cx, "settings-last-sync", &label);
    }

    #[gpui_kit::test]
    fn playback_steppers_write_synced_settings(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in);
        open_settings(&app, cx);
        app.wait_for_label(cx, "setting-speed", "1×");
        app.wait_for_label(cx, "setting-inbox-window", "30 days");
        scroll_to_playback(&app, cx);
        cx.update_window(app.window, |_, window, cx| {
            window.click("setting-speed-more", cx);
            window.click("setting-speed-more", cx);
            window.click("setting-inbox-window-less", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "setting-speed", "1.2×");
        app.wait_for_label(cx, "setting-inbox-window", "14 days");
        wait_for_value(
            &app,
            "select value from setting where key = 'playback_speed'",
            "1.2",
        );
        wait_for_value(
            &app,
            "select cast(json_extract(payload, '$.value') as text) from outbox
             where coalesce_key = 'setting:inbox_window_days'",
            "14",
        );
    }

    #[gpui_kit::test]
    fn every_click_of_a_fast_series_steps(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in);
        open_settings(&app, cx);
        app.wait_for_label(cx, "setting-inbox-window", "30 days");
        scroll_to_playback(&app, cx);
        cx.update_window(app.window, |_, window, cx| {
            window.double_click("setting-inbox-window-less", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "setting-inbox-window", "7 days");
        // Clicks far enough apart that each write lands and the page
        // reloads in between, as a person clicking at 30 ms does.
        for _ in 0..3 {
            cx.update_window(app.window, |_, window, cx| {
                window.click("setting-inbox-window-more", cx);
            })
            .expect("the window is still open");
            std::thread::sleep(Duration::from_millis(30));
            cx.run_until_parked();
        }
        app.wait_for_label(cx, "setting-inbox-window", "60 days");
        wait_for_value(
            &app,
            "select value from setting where key = 'inbox_window_days'",
            "60",
        );
    }

    #[gpui_kit::test]
    fn steppers_stop_at_the_ends_of_their_range(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in);
        open_settings(&app, cx);
        app.wait_for_label(cx, "setting-skip-back", "15 s");
        for _ in 0..3 {
            cx.update_window(app.window, |_, window, cx| {
                window.click("setting-skip-back-less", cx);
            })
            .expect("the window is still open");
            cx.run_until_parked();
        }
        app.wait_for_label(cx, "setting-skip-back", "5 s");
        cx.update_window(app.window, |_, window, _| {
            assert_eq!(window.find("setting-skip-back-less").disabled(), Some(true));
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn download_settings_stay_on_this_device(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in);
        open_settings(&app, cx);
        app.wait_for_label(cx, "setting-budget", "2 GiB");
        app.wait_for_label(cx, "setting-evict-grace", "24 h");
        cx.update_window(app.window, |_, window, cx| {
            // Downloads sits below the fold of the test window.
            window.scroll(
                "content",
                ScrollDelta::Pixels(point(px(0.), px(-2000.))),
                cx,
            );
        })
        .expect("the window is still open");
        app.wait_until(cx, "Downloads scrolls into view", |window| {
            window
                .try_find("setting-evict-grace-more")
                .is_some_and(|switch| switch.visible())
        });
        cx.update_window(app.window, |_, window, cx| {
            window.click("setting-budget-more", cx);
            window.click("setting-evict-grace-more", cx);
            window.click("setting-on-metered", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "setting-budget", "5 GiB");
        app.wait_for_label(cx, "setting-evict-grace", "2 days");
        wait_for_value(
            &app,
            "select value from device_setting where key = 'download_budget_bytes'",
            "5368709120",
        );
        wait_for_value(
            &app,
            "select value from device_setting where key = 'evict_grace_hours'",
            "48",
        );
        wait_for_value(
            &app,
            "select value from device_setting where key = 'download_on_metered'",
            "true",
        );
        let outbox: i64 = app.read(|connection| {
            connection
                .query_row("select count(*) from outbox", [], |row| row.get(0))
                .context("count the outbox")
        });
        assert_eq!(outbox, 0, "device settings never sync");
    }
}
