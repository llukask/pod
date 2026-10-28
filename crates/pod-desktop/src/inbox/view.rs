//! The inbox page (wireframe D1): the episode list and the selected
//! episode's detail pane with its show notes.
//!
//! Rows come from a live query, so they follow every write, local or
//! synced. Marking an episode played or archiving it hides its row at once
//! and writes in the background; the row stays hidden until a query result
//! leaves it out, or comes back if the write fails.

use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use jiff::tz::TimeZone;
use pod_protocol::{EpisodeId, Timestamp};

use super::{
    InboxEpisode, Loading, duration_label, load_inbox, reselect, step_selection, time_left_label,
};
use crate::downloads::view::{DownloadList, detail_control};
use crate::shell::artwork::artwork;
use crate::shell::episode_row;
use crate::show_notes::{self, ShowNotes};
use crate::store::{Store, live};
use crate::sync::SyncHandle;
use crate::sync::local_write::{self, EpisodeChange, LocalChange};

/// The key context of the inbox, for its key bindings.
const CONTEXT: &str = "Inbox";

gpui_kit::actions!(
    inbox,
    [SelectNext, SelectPrevious, MarkPlayed, Archive, Play]
);

/// Binds the inbox keys (D1 annotation 3), a nod to the TUI.
// TODO: `d` to download, once downloads exist.
pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", SelectNext, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("k", SelectPrevious, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("p", MarkPlayed, context),
        KeyBinding::new("e", Archive, context),
        KeyBinding::new("enter", Play, context),
    ]);
}

pub struct InboxView {
    focus_handle: FocusHandle,
    /// For the detail pane's download control.
    downloads: Entity<DownloadList>,
    /// The last query result.
    rows: Vec<InboxEpisode>,
    /// Rows hidden ahead of their played or archived write.
    leaving: HashSet<EpisodeId>,
    selected: Option<EpisodeId>,
    /// The selected episode's parsed notes, kept across renders.
    notes: Option<(EpisodeId, ShowNotes)>,
    scroll: UniformListScrollHandle,
    /// Sync progress while scopes are loading, for the empty list.
    loading: Option<Loading>,
    _query: Task<()>,
    _loading_query: Task<()>,
    _downloads_changed: Subscription,
}

impl std::fmt::Debug for InboxView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxView")
            .field("rows", &self.rows.len())
            .field("leaving", &self.leaving)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl InboxView {
    /// Requires the [`Store`] and [`SyncHandle`] globals. Takes the focus:
    /// the inbox is the first page, and its keys should work at once.
    pub fn new(
        downloads: Entity<DownloadList>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            focus_handle,
            _downloads_changed: cx.observe(&downloads, |_, _, cx| cx.notify()),
            downloads,
            rows: Vec::new(),
            leaving: HashSet::new(),
            selected: None,
            notes: None,
            scroll: UniformListScrollHandle::new(),
            loading: None,
            _query: live::observe_tables(
                cx,
                super::tables(),
                |connection| load_inbox(connection, Timestamp::now(), &TimeZone::system()),
                |this: &mut Self, rows, _| this.set_rows(rows),
            ),
            _loading_query: live::observe_tables(
                cx,
                super::loading_tables(),
                super::load_loading,
                |this: &mut Self, loading, _| this.loading = loading,
            ),
        }
    }

    fn visible(&self) -> impl Iterator<Item = &InboxEpisode> {
        self.rows
            .iter()
            .filter(|row| !self.leaving.contains(&row.id))
    }

    fn visible_ids(&self) -> Vec<EpisodeId> {
        self.visible().map(|row| row.id).collect()
    }

    fn selected_row(&self) -> Option<&InboxEpisode> {
        let selected = self.selected?;
        self.visible().find(|row| row.id == selected)
    }

    /// Changes what's listed with `change`, keeping the selection on a row
    /// that's still there.
    fn relist(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.visible_ids();
        change(self);
        self.selected = reselect(&before, &self.visible_ids(), self.selected);
    }

    fn set_rows(&mut self, rows: Vec<InboxEpisode>) {
        self.relist(|this| {
            this.rows = rows;
            let rows = &this.rows;
            this.leaving
                .retain(|id| rows.iter().any(|row| row.id == *id));
        });
    }

    fn select(&mut self, id: EpisodeId, cx: &mut Context<Self>) {
        self.selected = Some(id);
        if let Some(index) = self.visible().position(|row| row.id == id) {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        if let Some(id) = step_selection(&self.visible_ids(), self.selected, step) {
            self.select(id, cx);
        }
    }

    /// Hides the episode's row and writes `change` (played or archived) for
    /// it.
    fn leave(&mut self, id: EpisodeId, change: EpisodeChange, cx: &mut Context<Self>) {
        let Some(podcast_id) = self
            .visible()
            .find(|row| row.id == id)
            .map(|row| row.podcast_id)
        else {
            return;
        };
        self.relist(|this| {
            this.leaving.insert(id);
        });
        cx.notify();

        let store = cx.global::<Store>().clone();
        let sync = cx.global::<SyncHandle>().clone();
        let change = LocalChange::Episode {
            episode_id: id,
            podcast_id,
            change,
        };
        // The write runs on the store's writer thread; this task only waits
        // for its reply.
        cx.spawn(async move |this, cx| {
            if let Err(error) = local_write::write(&store, &sync, change).await {
                // TODO: tell the user, once the window has a place for errors.
                tracing::error!("marking episode {id} failed: {error:#}");
                let _ = this.update(cx, |this, cx| {
                    this.relist(|this| {
                        this.leaving.remove(&id);
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn leave_selected(&mut self, change: EpisodeChange, cx: &mut Context<Self>) {
        if let Some(id) = self.selected {
            self.leave(id, change, cx);
        }
    }

    /// Plays the selected episode, or resumes it if it is the one in the
    /// player.
    fn play_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected {
            crate::player::play(id, cx);
        }
    }
}

impl Focusable for InboxView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ==============================================================================
// Rendering
// ==============================================================================

impl Render for InboxView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("inbox")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_by(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.select_by(-1, cx)))
            .on_action(cx.listener(|this, _: &MarkPlayed, _, cx| {
                this.leave_selected(EpisodeChange::Played(true), cx);
            }))
            .on_action(cx.listener(|this, _: &Archive, _, cx| {
                this.leave_selected(EpisodeChange::Archived(true), cx);
            }))
            .on_action(cx.listener(|this, _: &Play, _, cx| this.play_selected(cx)))
            .child(self.render_list(cx))
            .child(self.render_detail(cx))
    }
}

impl InboxView {
    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.visible().count();
        let list = if let (0, Some(loading)) = (count, self.loading) {
            render_loading(loading, cx).into_any_element()
        } else if count == 0 {
            div()
                .id("inbox-empty")
                .test_support()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Nothing new. New episodes from your podcasts show up here.")
                .into_any_element()
        } else {
            uniform_list(
                "inbox-list",
                count,
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    let rows: Vec<InboxEpisode> = this
                        .visible()
                        .skip(range.start)
                        .take(range.len())
                        .cloned()
                        .collect();
                    rows.iter()
                        .map(|row| this.render_row(row, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element()
        };
        v_flex()
            .flex_none()
            .w(px(380.))
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div().px_4().py_3().child(
                    div()
                        .id("page-title")
                        .aria_label("Inbox")
                        .test_support()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Inbox"),
                ),
            )
            .child(list)
    }

    fn render_row(&self, row: &InboxEpisode, cx: &mut Context<Self>) -> ListItem {
        let id = row.id;
        let mut meta = vec![row.podcast_title.clone(), row.day.clone()];
        meta.extend(row.duration_ms.map(duration_label));
        let progress = time_left_label(row.duration_ms, row.position_ms).unwrap_or_default();
        ListItem::new(SharedString::from(format!("inbox-row-{id}")))
            .w_full()
            .h(px(72.))
            .px_4()
            .selected(self.selected == Some(id))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_handle.focus(window, cx);
                this.select(id, cx);
            }))
            .child(
                // ListItem's inner wrapper won't shrink below its content, so
                // a full-width clip with a zero-basis row inside keeps a long
                // title from pushing the suffix actions past the pane.
                h_flex().w_full().overflow_hidden().child(
                    h_flex()
                        .w_0()
                        .flex_1()
                        .overflow_hidden()
                        .gap_3()
                        .child(artwork(
                            SharedString::from(format!("inbox-artwork-{id}")),
                            row.artwork.as_deref(),
                            px(48.),
                            cx,
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(meta.join(" · ")),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("inbox-row-title-{id}")))
                                        .test_support()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .truncate()
                                        .child(row.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(progress),
                                ),
                        ),
                ),
            )
            .suffix({
                let view = cx.entity().downgrade();
                move |_, _| {
                    let view = view.clone();
                    episode_row::actions(id, false, false, move |change, cx| {
                        let _ = view.update(cx, |this, cx| this.leave(id, change, cx));
                    })
                }
            })
    }

    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.selected_row().cloned() else {
            return v_flex()
                .id("episode-detail")
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Select an episode")
                .into_any_element();
        };
        if self.notes.as_ref().is_none_or(|(id, _)| *id != row.id) {
            self.notes = Some((row.id, show_notes::parse(&row.show_notes_html)));
        }
        let notes = &self.notes.as_ref().expect("parsed just above").1;

        let mut meta = vec![row.day.clone()];
        meta.extend(row.duration_ms.map(duration_label));
        let play_label = match time_left_label(row.duration_ms, row.position_ms) {
            Some(left) => format!("Resume · {left}"),
            None => "Play".to_owned(),
        };
        let id = row.id;
        v_flex()
            .id("episode-detail")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .child(artwork(
                        "detail-artwork",
                        row.artwork.as_deref(),
                        px(96.),
                        cx,
                    ))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(row.podcast_title.clone()),
                            )
                            .child(
                                div()
                                    .id("detail-title")
                                    .aria_label(row.title.clone())
                                    .test_support()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(row.title.clone()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(meta.join(" · ")),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("detail-play")
                            .primary()
                            .icon(IconName::Play)
                            .label(play_label)
                            .on_click(cx.listener(|this, _, _, cx| this.play_selected(cx))),
                    )
                    .child(
                        Button::new("detail-played")
                            .outline()
                            .icon(IconName::Check)
                            .label("Played")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.leave(id, EpisodeChange::Played(true), cx);
                            })),
                    )
                    .child(
                        Button::new("detail-archive")
                            .outline()
                            .label("Archive")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.leave(id, EpisodeChange::Archived(true), cx);
                            })),
                    )
                    .child(detail_control(&self.downloads, id, row.podcast_id, cx)),
            )
            .child(div().h(px(1.)).bg(cx.theme().border))
            .child(show_notes::render::render("show-notes", notes, cx))
            .into_any_element()
    }
}

/// The empty list while the first sync fills the store (wireframe D6).
fn render_loading(loading: Loading, cx: &mut Context<InboxView>) -> impl IntoElement {
    let detail = if loading.shows_total == 0 {
        "Looking up your subscriptions".to_owned()
    } else {
        let shows = if loading.shows_total == 1 {
            "show"
        } else {
            "shows"
        };
        let episodes = if loading.episodes == 1 {
            "episode"
        } else {
            "episodes"
        };
        format!(
            "{} of {} {shows} · {} {episodes}",
            loading.shows_done, loading.shows_total, loading.episodes
        )
    };
    let fraction = if loading.shows_total == 0 {
        0.0
    } else {
        loading.shows_done as f32 / loading.shows_total as f32
    };
    v_flex()
        .id("inbox-loading")
        .aria_label(SharedString::from(detail.clone()))
        .test_support()
        .p_6()
        .gap_2()
        .items_center()
        .child(Spinner::new())
        .child(div().text_sm().child("Getting your podcasts"))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail),
        )
        .child(Progress::new("inbox-loading-progress").value(fraction * 100.0))
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::{Result, WrapErr as _};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, TestAppContext};
    use rusqlite::{Transaction, params};

    use crate::app::tests::TestApp;

    const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
    const NEWEST: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c01";
    const MIDDLE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c02";
    const OLDEST: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c03";
    const HOUR_MS: i64 = 60 * 60 * 1000;

    /// A signed-in account subscribed to one podcast with three inbox
    /// episodes, an hour apart, newest first.
    fn seed(transaction: &Transaction<'_>) -> Result<()> {
        let now = pod_protocol::Timestamp::now().as_epoch_ms();
        transaction
            .execute(
                "insert into account (id, server_url, user_id, username, device_id, token)
                 values (1, 'https://pod.example', '019a3f10-8c2e-7a41-9b3d-ffffffffffff',
                         'kilian', '019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d', 'secret')",
                [],
            )
            .context("insert the account")?;
        transaction
            .execute(
                "insert into podcast (id, feed_url, status, title, version)
                 values (?1, 'https://example.com/feed', 'ok', 'Darknet Diaries', 1)",
                [PODCAST],
            )
            .context("insert the podcast")?;
        transaction
            .execute(
                "insert into subscription
                     (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
                 values (?1, 'https://example.com/feed', 1, ?2, 'dev')",
                params![PODCAST, now - 240 * HOUR_MS],
            )
            .context("insert the subscription")?;
        for (hours_ago, id, title) in [
            (1, NEWEST, "EP 151: The pager"),
            (2, MIDDLE, "EP 150: Grifter"),
            (3, OLDEST, "EP 149: Mini-stories"),
        ] {
            transaction
                .execute(
                    "insert into episode (id, podcast_id, title, summary_html, content_html,
                         published_at, duration_ms, audio_url, audio_type, version)
                     values (?1, ?2, ?3, '', '<p>Notes for <b>' || ?3 || '</b></p>', ?4,
                             3120000, 'https://example.com/a.mp3', 'audio/mpeg', 1)",
                    params![id, PODCAST, title, now - hours_ago * HOUR_MS],
                )
                .context("insert an episode")?;
        }
        Ok(())
    }

    fn row(id: &str) -> String {
        format!("inbox-row-{id}")
    }

    fn unplayed_badge() -> String {
        format!("podcast-unplayed-{PODCAST}")
    }

    /// `(played, archived)` of an episode's state row, if it has one.
    fn state(app: &TestApp, id: &'static str) -> Option<(bool, bool)> {
        app.read(move |connection| {
            use rusqlite::OptionalExtension as _;
            connection
                .query_row(
                    "select played, archived from episode_state where episode_id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .context("read the episode state")
        })
    }

    #[gpui_kit::test]
    fn archiving_from_the_inbox_removes_the_row_at_once(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed);
        app.wait_until(cx, "the inbox lists the episodes", |window| {
            window.try_find(row(OLDEST)).is_some()
        });
        app.wait_for_label(cx, &unplayed_badge(), "3");

        cx.update_window(app.window, |_, window, cx| {
            assert_eq!(
                window.find("detail-title").label(),
                Some("EP 151: The pager")
            );

            window.press("e", cx);
            assert!(
                window.try_find(row(NEWEST)).is_none(),
                "the row goes before the write lands"
            );
            assert_eq!(
                window.find("detail-title").label(),
                Some("EP 150: Grifter"),
                "the next row takes over the selection"
            );

            window.click(format!("row-archive-{OLDEST}"), cx);
            assert!(window.try_find(row(OLDEST)).is_none());
            assert!(window.try_find(row(MIDDLE)).is_some());
        })
        .expect("the window is still open");

        app.wait_for_label(cx, &unplayed_badge(), "1");
        assert_eq!(state(&app, NEWEST), Some((false, true)));
        assert_eq!(state(&app, OLDEST), Some((false, true)));
        assert_eq!(state(&app, MIDDLE), None);
        cx.update_window(app.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(row(NEWEST)).is_none(), "the query agrees");
            assert!(window.try_find(row(OLDEST)).is_none());
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn the_detail_pane_downloads_and_cancels(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed);
        app.wait_until(cx, "the detail pane offers a download", |window| {
            window.try_find("detail-download").is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            window.click("detail-download", cx);
        })
        .expect("the window is still open");
        // The reconciler isn't running, so the pin waits for its row.
        app.wait_for_label(cx, "detail-download-status", "Queued");
        let pins = || {
            app.read(|connection| {
                connection
                    .query_row(
                        "select count(*) from download_wish where episode_id = ?1 and source = 'pin'",
                        [NEWEST],
                        |row| row.get::<_, i64>(0),
                    )
                    .context("count the pins")
            })
        };
        assert_eq!(pins(), 1);

        cx.update_window(app.window, |_, window, cx| {
            window.click(format!("detail-download-cancel-{NEWEST}"), cx);
        })
        .expect("the window is still open");
        app.wait_until(cx, "the download button is back", |window| {
            window.try_find("detail-download").is_some()
        });
        assert_eq!(pins(), 0);
        assert_eq!(
            state(&app, NEWEST),
            None,
            "cancelling a pin doesn't archive"
        );
    }

    #[gpui_kit::test]
    fn a_long_title_truncates_inside_its_row(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, |transaction| {
            seed(transaction)?;
            transaction
                .execute(
                    "update episode set title = ?1 where id = ?2",
                    params![
                        "Episode 6: The one with an extremely long title that keeps going \
                         well past where any reasonable layout would want it to stop",
                        NEWEST
                    ],
                )
                .context("lengthen a title")?;
            Ok(())
        });
        app.wait_until(cx, "the inbox lists the episodes", |window| {
            window.try_find(row(NEWEST)).is_some()
        });
        cx.update_window(app.window, |_, window, _| {
            let long = window.find(row(NEWEST)).bounds();
            let short = window.find(row(MIDDLE)).bounds();
            let title = window.find(format!("inbox-row-title-{NEWEST}")).bounds();
            assert_eq!(
                long.size.width, short.size.width,
                "a long title doesn't widen its row past the pane"
            );
            assert!(
                title.size.width > gpui_kit::px(100.),
                "the title {title:?} keeps its share of the row"
            );
            assert!(
                title.right() <= long.right() - gpui_kit::px(16.),
                "the title {title:?} stops at the row's padding {long:?}"
            );
            let archive = window.find(format!("row-archive-{NEWEST}")).bounds();
            assert!(
                archive.right() <= long.right(),
                "the long row's actions {archive:?} stay inside it {long:?}"
            );
            assert_eq!(
                archive.origin.x,
                window
                    .find(format!("row-archive-{MIDDLE}"))
                    .bounds()
                    .origin
                    .x,
                "every row's actions line up"
            );
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn keys_move_the_selection_and_mark_played(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed);
        app.wait_until(cx, "the inbox lists the episodes", |window| {
            window.try_find(row(OLDEST)).is_some()
        });

        cx.update_window(app.window, |_, window, cx| {
            window.press("j", cx);
            window.press("down", cx);
            assert_eq!(
                window.find("detail-title").label(),
                Some("EP 149: Mini-stories")
            );
            window.press("j", cx);
            assert_eq!(
                window.find("detail-title").label(),
                Some("EP 149: Mini-stories"),
                "the selection stops at the last row"
            );
            window.press("k", cx);
            assert_eq!(window.find("detail-title").label(), Some("EP 150: Grifter"));

            window.press("p", cx);
            assert!(window.try_find(row(MIDDLE)).is_none());
            assert_eq!(
                window.find("detail-title").label(),
                Some("EP 149: Mini-stories")
            );
        })
        .expect("the window is still open");

        app.wait_for_label(cx, &unplayed_badge(), "2");
        assert_eq!(state(&app, MIDDLE), Some((true, false)));
    }
}
