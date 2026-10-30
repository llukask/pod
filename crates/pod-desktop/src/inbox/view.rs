//! The inbox page (wireframe D1): the episode list, grouped by day, and the
//! selected episode's detail pane with its show notes.
//!
//! Rows come from a live query, so they follow every write, local or
//! synced. Marking an episode played or archiving it hides its row at once
//! and writes in the background; the row stays hidden until a query result
//! leaves it out, or comes back if the write fails.
//!
//! An episode is always selected while there are any, as in a mail client:
//! the reading pane beside the list shows the newest until another is
//! chosen, and marking it played or archiving it moves on to the next.

use std::collections::HashSet;

use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, ListAlignment, ListState, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div, list, px,
};
use jiff::tz::TimeZone;
use pod_protocol::{EpisodeId, PodcastId, Timestamp};

use super::{
    DayGroup, InboxEpisode, Loading, duration_label, load_inbox, reselect, step_selection,
    time_left_label,
};
use crate::downloads::view::{DownloadList, detail_control};
use crate::player::PlayerHandle;
use crate::search::view::FocusSearch;
use crate::shell::episode_row::{self, Detail, Row, detail_header, detail_pane, length_label};
use crate::shell::icon::PodIcon;
use crate::shell::layout::{self, CONTENT_MAX, GUTTER};
use crate::shell::ui::{
    Ring, empty_state, icon, page_title, pill, primary_pill, quiet_pill, small_heading,
};
use crate::show_notes::{self, ShowNotes};
use crate::store::{Store, live};
use crate::sync::SyncHandle;
use crate::sync::local_write::{self, EpisodeChange, LocalChange};
use crate::theme::palette;

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

/// The detail pane's show link asks the window to open the show's page.
#[derive(Debug, Clone, Copy)]
pub struct OpenPodcast(pub PodcastId);

/// One entry of the list: a day's heading, or a row by its index among the
/// visible rows. The list is newest first, so each day is one run of rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Heading(DayGroup),
    Row(usize),
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
    /// The headings and visible rows, in list order.
    items: Vec<Item>,
    list: ListState,
    /// Sync progress while scopes are loading, for the empty list.
    loading: Option<Loading>,
    _query: Task<()>,
    _loading_query: Task<()>,
    _downloads_changed: Subscription,
    /// Redraws the rows' play buttons as the player moves.
    _player_changed: Option<Subscription>,
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

impl EventEmitter<OpenPodcast> for InboxView {}

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
        let player_changed = cx
            .try_global::<PlayerHandle>()
            .cloned()
            .map(|PlayerHandle(player)| cx.observe(&player, |_, _, cx| cx.notify()));
        Self {
            focus_handle,
            _downloads_changed: cx.observe(&downloads, |_, _, cx| cx.notify()),
            downloads,
            rows: Vec::new(),
            leaving: HashSet::new(),
            selected: None,
            notes: None,
            items: Vec::new(),
            // Overdraw keeps a few rows past the edges measured, so fast
            // scrolling doesn't pop rows in.
            list: ListState::new(0, ListAlignment::Top, px(400.)),
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
            _player_changed: player_changed,
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
        self.rebuild_items();
    }

    /// Recomputes the list's entries and tells the list state, keeping the
    /// scroll position: rows change on every write, a position save
    /// included.
    fn rebuild_items(&mut self) {
        let mut items = Vec::new();
        let mut group = None;
        for (index, row) in self.visible().enumerate() {
            if group != Some(row.group) {
                group = Some(row.group);
                items.push(Item::Heading(row.group));
            }
            items.push(Item::Row(index));
        }
        let (old, new) = (self.items.len(), items.len());
        let kept = old.min(new);
        self.list.splice(kept..old, new - kept);
        self.list.remeasure_items(0..kept);
        self.items = items;
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
        if let Some(index) = self.visible().position(|row| row.id == id)
            && let Some(item) = self.items.iter().position(|item| *item == Item::Row(index))
        {
            self.list.scroll_to_reveal_item(item);
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let split = layout::split(window.viewport_size().width);
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
            .child(self.render_list(split, cx))
            .when_some(split.detail, |this, width| {
                this.child(
                    div()
                        .flex_none()
                        .w(width)
                        .h_full()
                        .child(self.render_detail(cx)),
                )
            })
    }
}

impl InboxView {
    fn render_list(&self, split: layout::Split, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let count = self.visible().count();
        let columns = layout::has_columns(split.list);
        let body = if let (0, Some(loading)) = (count, self.loading) {
            render_loading(loading, cx).into_any_element()
        } else if count == 0 {
            empty_state(
                "inbox-empty",
                PodIcon::Inbox,
                "You're all caught up",
                "New episodes from your podcasts land here.",
                Some(
                    pill("inbox-add-podcast", cx)
                        .icon(icon(PodIcon::Plus, px(18.)))
                        .label("Add a podcast")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(FocusSearch), cx))
                        .into_any_element(),
                ),
                cx,
            )
            .into_any_element()
        } else {
            // The heading of the day at the top stays in view while its rows
            // scroll under it, as CSS `position: sticky` would keep it.
            let top = self.list.logical_scroll_top();
            let sticky = self.items[..=top.item_ix.min(self.items.len() - 1)]
                .iter()
                .rev()
                .find_map(|item| match item {
                    Item::Heading(group) => Some(*group),
                    Item::Row(_) => None,
                })
                .filter(|_| top.item_ix > 0 || top.offset_in_item > px(0.));
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    list(
                        self.list.clone(),
                        cx.processor(move |this, index: usize, _, cx| {
                            this.render_item(index, columns, cx)
                        }),
                    )
                    .size_full()
                    .pb(px(24.)),
                )
                .when_some(sticky, |this, group| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .bg(p.background)
                            .child(group_heading(group, cx)),
                    )
                })
                .into_any_element()
        };
        v_flex()
            .id("inbox-list-pane")
            .test_support()
            .flex_none()
            .w(split.list)
            .h_full()
            .child(
                div()
                    .w_full()
                    .max_w(CONTENT_MAX)
                    .mx_auto()
                    .px(GUTTER)
                    .pt(px(32.))
                    .pb(px(4.))
                    .child(page_title("Inbox")),
            )
            .child(body)
    }

    fn render_item(&mut self, index: usize, columns: bool, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.items.get(index) {
            Some(Item::Heading(group)) => group_heading(*group, cx).into_any_element(),
            Some(Item::Row(row)) => self.render_row(*row, index, columns, cx),
            None => div().into_any_element(),
        };
        div()
            .w_full()
            .child(div().w_full().max_w(CONTENT_MAX).mx_auto().child(content))
            .into_any_element()
    }

    fn render_row(
        &self,
        row_index: usize,
        item_index: usize,
        columns: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.visible().nth(row_index).cloned() else {
            return div().into_any_element();
        };
        let id = row.id;
        let first = matches!(
            item_index.checked_sub(1).map(|ix| self.items[ix]),
            Some(Item::Heading(_)) | None
        );
        let after_highlight = !first
            && matches!(self.items[item_index - 1], Item::Row(previous)
                if self.visible().nth(previous).is_some_and(|row| Some(row.id) == self.selected));
        let mut ring = Ring::new(row.position_ms, row.duration_ms, false);
        if let Some((playing_id, playing, position_ms)) = crate::player::now_playing(cx)
            && playing_id == id
        {
            ring = Ring::new(position_ms, row.duration_ms, false);
            ring.current = true;
            ring.playing = playing;
        }
        let view = cx.entity().downgrade();
        div()
            .px(GUTTER - px(12.))
            .child(episode_row::row(
                Row {
                    id,
                    prefix: "inbox",
                    show: Some((row.podcast_title.clone(), row.artwork.clone())),
                    title: row.title.clone(),
                    when: (!row.group.says_when()).then(|| row.day.clone()),
                    length: length_label(row.duration_ms, row.position_ms, false),
                    state_label: None,
                    played: false,
                    archived: false,
                    ring,
                    selected: self.selected == Some(id),
                    after_highlight,
                    first,
                    columns,
                },
                cx.listener(move |this, _, window, cx| {
                    this.focus_handle.focus(window, cx);
                    this.select(id, cx);
                }),
                move |change, cx| {
                    let _ = view.update(cx, |this, cx| this.leave(id, change, cx));
                },
                cx,
            ))
            .into_any_element()
    }

    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.selected_row().cloned() else {
            return div()
                .id("episode-detail")
                .size_full()
                .bg(palette(cx).surface)
                .into_any_element();
        };
        if self.notes.as_ref().is_none_or(|(id, _)| *id != row.id) {
            self.notes = Some((row.id, show_notes::parse(&row.show_notes_html)));
        }
        let notes = &self.notes.as_ref().expect("parsed just above").1;

        let mut meta = vec![row.day.clone()];
        meta.extend(row.duration_ms.map(duration_label));
        let id = row.id;
        let playing = crate::player::now_playing(cx)
            .is_some_and(|(playing_id, playing, _)| playing_id == id && playing);
        let play_label = if playing {
            "Pause".to_owned()
        } else {
            match time_left_label(row.duration_ms, row.position_ms) {
                Some(left) => format!("Resume · {left}"),
                None => "Play".to_owned(),
            }
        };
        let detail = Detail {
            artwork: row.artwork.clone(),
            podcast_title: row.podcast_title.clone(),
            title: row.title.clone(),
            meta: meta.join(" · "),
        };
        let podcast_id = row.podcast_id;
        let header = detail_header(
            &detail,
            Some(Box::new(cx.listener(move |_, _, _, cx| {
                cx.emit(OpenPodcast(podcast_id));
            }))),
            cx,
        );
        let actions = h_flex()
            .flex_wrap()
            .gap(px(8.))
            .child(
                primary_pill("detail-play", cx)
                    .pl(px(14.))
                    .icon(icon(
                        if playing {
                            PodIcon::Pause
                        } else {
                            PodIcon::Play
                        },
                        px(18.),
                    ))
                    .label(play_label)
                    .on_click(move |_, _, cx| crate::player::play_or_pause(id, cx)),
            )
            .child(
                pill("detail-played", cx)
                    .icon(icon(PodIcon::Check, px(18.)))
                    .label("Mark played")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.leave(id, EpisodeChange::Played(true), cx);
                    })),
            )
            .child(
                pill("detail-archive", cx)
                    .icon(icon(PodIcon::Archive, px(18.)))
                    .label("Archive")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.leave(id, EpisodeChange::Archived(true), cx);
                    })),
            )
            .child(detail_control(&self.downloads, id, row.podcast_id, cx))
            .when_some(row.link.clone(), |this, link| {
                this.child(
                    quiet_pill("detail-link", cx)
                        .icon(icon(PodIcon::External, px(18.)))
                        .label("Episode page")
                        .on_click(move |_, _, cx| cx.open_url(&link)),
                )
            });
        detail_pane(
            header,
            actions,
            show_notes::render::render("show-notes", notes, cx).into_any_element(),
            cx,
        )
        .into_any_element()
    }
}

/// "Today", "Yesterday", …: a day's heading above its rows.
fn group_heading(group: DayGroup, cx: &App) -> impl IntoElement {
    div()
        .w_full()
        .max_w(CONTENT_MAX)
        .mx_auto()
        .px(GUTTER)
        .pt(px(18.))
        .pb(px(6.))
        .child(small_heading(group.label(), cx))
}

/// The empty list while the first sync fills the store (wireframe D6).
fn render_loading(loading: Loading, cx: &mut Context<InboxView>) -> impl IntoElement {
    let p = palette(cx);
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
        .w_full()
        .px(px(24.))
        .py(px(72.))
        .gap(px(8.))
        .items_center()
        .child(Spinner::new().color(p.accent))
        .child(
            div()
                .mt(px(8.))
                .font_family(crate::theme::FONT_DISPLAY)
                .text_size(crate::theme::size::TITLE)
                .font_weight(FontWeight::SEMIBOLD)
                .child("Getting your podcasts"),
        )
        .child(div().text_color(p.muted).child(detail))
        .child(
            div()
                .w(px(240.))
                .mt(px(8.))
                .child(Progress::new("inbox-loading-progress").value(fraction * 100.0)),
        )
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

            // The row's buttons show while the pointer is over it.
            window.hover(row(OLDEST), cx);
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
