//! The podcast page (wireframes D2 and D7): a header with the feed's state
//! and actions, this device's settings in a popover, every episode newest
//! first, and the selected episode's detail pane with its show notes.
//!
//! Rows come from a live query, so they follow every write. Unlike the
//! inbox, rows stay when marked played or archived; their state shows.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, KeyBinding, ListAlignment, ListState, ParentElement as _, Pixels, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div, list, px, relative,
};
use jiff::tz::TimeZone;
use pod_protocol::{EpisodeId, PodcastId, Timestamp};

use super::{
    AUTO_KEEP_MAX, DeviceSettings, PodcastEpisode, PodcastPage, PodcastState, load_page,
    load_show_notes, mark_all_played_title, progress_label, status_notice, unsubscribe_message,
};
use crate::downloads::view::{DownloadList, detail_control};
use crate::inbox::view::{Archive, MarkPlayed, Play, SelectNext, SelectPrevious};
use crate::inbox::{duration_label, reselect, step_selection, time_left_label};
use crate::player::PlayerHandle;
use crate::runtime::Background;
use crate::shell::artwork::artwork;
use crate::shell::episode_row::{self, Detail, Row, detail_header, detail_pane, length_label};
use crate::shell::icon::PodIcon;
use crate::shell::layout::{self, CONTENT_MAX, GUTTER, SIDE_COLUMN};
use crate::shell::ui::{
    ROUND, Ring, art_shadow, danger_pill, dialog_title, icon, icon_button, page_title, pill,
    primary_pill, quiet_pill, segmented,
};
use crate::shell::wash;
use crate::show_notes::{self, ShowNotes};
use crate::store::{Store, live};
use crate::sync::SyncHandle;
use crate::sync::local_write::{self, EpisodeChange, LocalChange};
use crate::theme::{mix, palette, size};

/// The key context of the podcast page.
const CONTEXT: &str = "Podcast";

/// Binds the inbox's keys on this page too, where `p` and `e` toggle.
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

pub struct PodcastView {
    id: PodcastId,
    focus_handle: FocusHandle,
    /// For the detail pane's download control.
    downloads: Entity<DownloadList>,
    /// The last query result. `None` before the first one arrives, and
    /// once the podcast is no longer subscribed; the app then leaves the
    /// page.
    page: Option<PodcastPage>,
    selected: Option<EpisodeId>,
    /// The selected episode's show notes, once asked for.
    notes: Option<Notes>,
    /// All episodes, or only the unplayed ones.
    filter: Filter,
    /// The show's description unclamped.
    expanded: bool,
    /// The header and filters, then the listed episodes.
    list: ListState,
    /// Whether the header was last drawn as a side column, which changes
    /// the list's first entry.
    side_column: bool,
    /// The list pane's width when last drawn, which the header follows.
    pane_width: Pixels,
    _query: Task<()>,
    _downloads_changed: Subscription,
    /// Redraws the rows' play buttons as the player moves.
    _player_changed: Option<Subscription>,
}

/// The All / Unplayed control above the episodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Unplayed,
}

/// One episode's show notes, read and parsed on the store's read thread.
struct Notes {
    episode: EpisodeId,
    /// `None` while the read runs.
    parsed: Option<ShowNotes>,
    /// Dropping it, on the next selection, cancels a read still running.
    _load: Task<()>,
}

impl std::fmt::Debug for PodcastView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PodcastView")
            .field("id", &self.id)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl PodcastView {
    /// Requires the [`Store`], [`SyncHandle`] and [`Background`] globals.
    /// Takes the focus, so the page's keys work at once.
    pub fn new(
        id: PodcastId,
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
            id,
            focus_handle,
            _downloads_changed: cx.observe(&downloads, |_, _, cx| cx.notify()),
            downloads,
            page: None,
            selected: None,
            notes: None,
            filter: Filter::All,
            expanded: false,
            list: ListState::new(1, ListAlignment::Top, px(600.)),
            side_column: false,
            pane_width: px(0.),
            _player_changed: player_changed,
            _query: live::observe_tables(
                cx,
                super::tables(),
                move |connection| load_page(connection, id, Timestamp::now(), &TimeZone::system()),
                |this: &mut Self, page, _| this.set_page(page),
            ),
        }
    }

    /// Takes a query result. The first one selects the newest episode, as
    /// the inbox does; later ones keep the selection, since rows only leave
    /// when the feed drops an episode.
    fn set_page(&mut self, page: Option<PodcastPage>) {
        self.relist(|this| this.page = page);
    }

    /// Changes what's listed with `change`, keeping the selection on a row
    /// that's still there and the list's scroll position.
    fn relist(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.listed_ids();
        change(self);
        let after = self.listed_ids();
        self.selected = reselect(&before, &after, self.selected);
        // The first entry is the header; the rows follow.
        let (old, new) = (before.len() + 1, after.len() + 1);
        let kept = old.min(new);
        self.list.splice(kept..old, new - kept);
        self.list.remeasure_items(0..kept);
    }

    /// The episodes the filter lets through, newest first.
    fn listed(&self) -> impl Iterator<Item = &PodcastEpisode> {
        let unplayed_only = self.filter == Filter::Unplayed;
        self.episodes()
            .iter()
            .filter(move |episode| !unplayed_only || !episode.played)
    }

    fn listed_ids(&self) -> Vec<EpisodeId> {
        self.listed().map(|episode| episode.id).collect()
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.relist(|this| this.filter = filter);
            cx.notify();
        }
    }

    pub fn podcast_id(&self) -> PodcastId {
        self.id
    }

    fn episodes(&self) -> &[PodcastEpisode] {
        self.page.as_ref().map_or(&[], |page| &page.episodes)
    }

    fn episode(&self, id: EpisodeId) -> Option<&PodcastEpisode> {
        self.episodes().iter().find(|episode| episode.id == id)
    }

    fn select(&mut self, id: EpisodeId, cx: &mut Context<Self>) {
        self.selected = Some(id);
        if let Some(index) = self.listed().position(|row| row.id == id) {
            self.list.scroll_to_reveal_item(index + 1);
        }
        cx.notify();
    }

    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        if let Some(id) = step_selection(&self.listed_ids(), self.selected, step) {
            self.select(id, cx);
        }
    }

    /// Writes `change` for episode `id`. The row follows once the query
    /// re-runs.
    fn change(&mut self, id: EpisodeId, change: EpisodeChange, cx: &mut Context<Self>) {
        let store = cx.global::<Store>().clone();
        let sync = cx.global::<SyncHandle>().clone();
        let change = LocalChange::Episode {
            episode_id: id,
            podcast_id: self.id,
            change,
        };
        cx.spawn(async move |_, _| {
            if let Err(error) = local_write::write(&store, &sync, change).await {
                // TODO: tell the user, once the window has a place for errors.
                tracing::error!("changing episode {id} failed: {error:#}");
            }
        })
        .detach();
    }

    /// Flips the selected episode's played or archived flag.
    fn toggle_selected(&mut self, played: bool, cx: &mut Context<Self>) {
        let Some(episode) = self.selected.and_then(|id| self.episode(id)) else {
            return;
        };
        let (id, change) = if played {
            (episode.id, EpisodeChange::Played(!episode.played))
        } else {
            (episode.id, EpisodeChange::Archived(!episode.archived))
        };
        self.change(id, change, cx);
    }

    /// Starts reading episode `id`'s show notes unless they are already
    /// read or being read.
    fn load_notes(&mut self, id: EpisodeId, cx: &mut Context<Self>) {
        if self.notes.as_ref().is_some_and(|notes| notes.episode == id) {
            return;
        }
        let store = cx.global::<Store>().clone();
        let load = cx.spawn(async move |this, cx| {
            let parsed = store
                .read(move |connection| load_show_notes(connection, id))
                .await
                .unwrap_or_else(|error| {
                    tracing::error!("reading episode {id}'s show notes failed: {error:#}");
                    ShowNotes::default()
                });
            let _ = this.update(cx, |this, cx| {
                if let Some(notes) = this.notes.as_mut().filter(|notes| notes.episode == id) {
                    notes.parsed = Some(parsed);
                    cx.notify();
                }
            });
        });
        self.notes = Some(Notes {
            episode: id,
            parsed: None,
            _load: load,
        });
    }

    /// Asks before marking every episode played, with how many. It asks
    /// rather than offering an undo: marking played releases the episodes'
    /// pins and auto downloads, and the reconciler may delete the files
    /// before an undo could put the flags back.
    fn confirm_mark_all_played(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = &self.page else {
            return;
        };
        let unplayed = page
            .episodes
            .iter()
            .filter(|episode| !episode.played)
            .count();
        let title = SharedString::from(mark_all_played_title(
            u32::try_from(unplayed).unwrap_or(u32::MAX),
        ));
        let message = SharedString::from(
            "They leave the inbox, and their downloads are removed from this computer.",
        );
        let id = self.id;
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(
                    div()
                        .id("mark-all-played-title")
                        .aria_label(title.clone())
                        .test_support()
                        .child(dialog_title(title.clone())),
                )
                .child(
                    div()
                        .id("mark-all-played-message")
                        .aria_label(message.clone())
                        .test_support()
                        .text_sm()
                        .child(message.clone()),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|button| button.rounded(ROUND).label("Cancel")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("mark-all-played-confirm")
                                    .primary()
                                    .rounded(ROUND)
                                    .label("Mark played"),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let store = cx.global::<Store>().clone();
                    let sync = cx.global::<SyncHandle>().clone();
                    cx.global::<Background>().spawn(async move {
                        if let Err(error) = super::mark_all_played(&store, &sync, id).await {
                            tracing::error!("marking podcast {id} played failed: {error:#}");
                        }
                    });
                    true
                })
        });
    }

    /// Shows `settings` at once and writes them in the background, so two
    /// quick clicks on the stepper both count.
    fn set_device(&mut self, settings: DeviceSettings, cx: &mut Context<Self>) {
        let Some(page) = &mut self.page else {
            return;
        };
        page.device = settings;
        cx.notify();
        let store = cx.global::<Store>().clone();
        let id = self.id;
        cx.spawn(async move |_, _| {
            if let Err(error) = super::set_device_settings(&store, id, settings).await {
                tracing::error!("saving podcast {id}'s device settings failed: {error:#}");
            }
        })
        .detach();
    }

    /// Asks before unsubscribing, with what goes (D7 annotation 3). The
    /// local write takes the show out of the sidebar at once; the next sync
    /// tells the server and the other devices, and drops the catalog rows,
    /// after which the reconciler removes the downloads (spec 7.4).
    fn confirm_unsubscribe(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = &self.page else {
            return;
        };
        let title = SharedString::from(format!("Unsubscribe from {}?", page.title));
        let message = SharedString::from(unsubscribe_message(page.downloads));
        let change = LocalChange::Subscription {
            podcast_id: page.id,
            feed_url: page.feed_url.clone(),
            subscribed: false,
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let change = change.clone();
            dialog
                .title(dialog_title(title.clone()))
                .child(
                    div()
                        .id("unsubscribe-message")
                        .aria_label(message.clone())
                        .test_support()
                        .text_sm()
                        .child(message.clone()),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|button| button.rounded(ROUND).label("Cancel")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("unsubscribe-confirm")
                                    .danger()
                                    .rounded(ROUND)
                                    .label("Unsubscribe"),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let store = cx.global::<Store>().clone();
                    let sync = cx.global::<SyncHandle>().clone();
                    let change = change.clone();
                    // The sidebar follows the subscription row, so nothing
                    // waits for this.
                    cx.global::<Background>().spawn(async move {
                        if let Err(error) = local_write::write(&store, &sync, change).await {
                            tracing::error!("unsubscribing failed: {error:#}");
                        }
                    });
                    true
                })
        });
    }
}

impl Focusable for PodcastView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ==============================================================================
// Rendering
// ==============================================================================

impl Render for PodcastView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = h_flex()
            .id("podcast")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .flex_1()
            .min_w_0()
            .h_full()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_by(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.select_by(-1, cx)))
            .on_action(cx.listener(|this, _: &MarkPlayed, _, cx| this.toggle_selected(true, cx)))
            .on_action(cx.listener(|this, _: &Archive, _, cx| this.toggle_selected(false, cx)))
            .on_action(cx.listener(|this, _: &Play, _, cx| {
                if let Some(id) = this.selected {
                    crate::player::play(id, cx);
                }
            }));
        let Some(page) = self.page.clone() else {
            return root;
        };
        let split = layout::split(window.viewport_size().width);
        let side_column = layout::has_side_column(split.list);
        if side_column != self.side_column || split.list != self.pane_width {
            self.side_column = side_column;
            self.pane_width = split.list;
            self.list.remeasure_items(0..1);
        }
        let columns = layout::has_columns(if side_column {
            split.list - SIDE_COLUMN
        } else {
            split.list
        });
        let p = palette(cx);

        let pane = div()
            .id("podcast-list-pane")
            .relative()
            .flex_none()
            .w(split.list)
            .h_full()
            .overflow_hidden();
        let pane = if page.episodes.is_empty() {
            // Nothing to scroll: the header, then why there are no episodes.
            pane.children(wash::header_layer(page.artwork.as_deref(), WASH_HEIGHT, cx))
                .child(
                    v_flex()
                        .relative()
                        .size_full()
                        .child(self.render_header(&page, false, cx))
                        .child(self.render_empty(&page, cx)),
                )
        } else if side_column {
            pane.children(wash::header_layer(page.artwork.as_deref(), WASH_HEIGHT, cx))
                .child(
                    h_flex()
                        .relative()
                        .size_full()
                        .max_w(CONTENT_MAX)
                        .mx_auto()
                        .items_start()
                        .child(
                            div()
                                .id("podcast-side-column")
                                .flex_none()
                                .w(SIDE_COLUMN)
                                .h_full()
                                .overflow_y_scroll()
                                .child(self.render_header(&page, true, cx)),
                        )
                        .child(
                            div()
                                .id("podcast-list-column")
                                .test_support()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .child(self.render_list(columns, cx)),
                        ),
                )
        } else {
            pane.child(
                div()
                    .id("podcast-list-column")
                    .test_support()
                    .size_full()
                    .child(self.render_list(columns, cx)),
            )
        };
        root.child(pane).when_some(
            split.detail.filter(|_| !page.episodes.is_empty()),
            |root, width| {
                root.child(
                    div()
                        .flex_none()
                        .w(width)
                        .h_full()
                        .bg(p.surface)
                        .child(self.render_detail(cx)),
                )
            },
        )
    }
}

impl PodcastView {
    /// The list: its first entry is the header (or, beside a side column,
    /// just the filters), then the rows.
    fn render_list(&self, columns: bool, cx: &mut Context<Self>) -> impl IntoElement {
        list(
            self.list.clone(),
            cx.processor(move |this, index: usize, _, cx| {
                let Some(page) = this.page.clone() else {
                    return div().into_any_element();
                };
                if index == 0 {
                    let top = v_flex().w_full().max_w(CONTENT_MAX).mx_auto();
                    return if this.side_column {
                        top.pt(px(32.))
                            .child(this.render_filters(&page, cx))
                            .into_any_element()
                    } else {
                        div()
                            .relative()
                            .w_full()
                            .children(wash::header_layer(page.artwork.as_deref(), WASH_HEIGHT, cx))
                            .child(
                                top.relative()
                                    .child(this.render_header(&page, false, cx))
                                    .child(this.render_filters(&page, cx)),
                            )
                            .into_any_element()
                    };
                }
                this.render_row(index - 1, columns, cx)
            }),
        )
        .size_full()
        .pb(px(24.))
    }

    fn render_filters(&self, page: &PodcastPage, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let any_unplayed = page.episodes.iter().any(|episode| !episode.played);
        h_flex()
            .px(GUTTER)
            .pt(px(8.))
            .pb(px(8.))
            .gap(px(8.))
            .child(segmented(
                &[
                    ("podcast-filter-all", "All", self.filter == Filter::All),
                    (
                        "podcast-filter-unplayed",
                        "Unplayed",
                        self.filter == Filter::Unplayed,
                    ),
                ],
                move |index, _, cx| {
                    let filter = if index == 0 {
                        Filter::All
                    } else {
                        Filter::Unplayed
                    };
                    let _ = view.update(cx, |this, cx| this.set_filter(filter, cx));
                },
                cx,
            ))
            .child(div().flex_1())
            .child(
                quiet_pill("mark-all-played", cx)
                    .h(px(32.))
                    .icon(icon(PodIcon::Check, px(16.)))
                    .label("Mark all played")
                    .disabled(!any_unplayed)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.confirm_mark_all_played(window, cx);
                    })),
            )
    }

    /// The show's header. `column` lays it out as the wide page's side
    /// column: the cover the column's width, a smaller title, a longer
    /// description. A narrow pane stacks it the same way, with a smaller
    /// cover, since the title beside the cover would get a few letters a
    /// line.
    fn render_header(
        &self,
        page: &PodcastPage,
        column: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let episodes = u32::try_from(page.episodes.len()).unwrap_or(u32::MAX);
        let notice = status_notice(
            &page.state,
            episodes,
            page.downloads.total,
            Timestamp::now(),
        );
        let stacked = !column && self.pane_width < STACK_BELOW;
        let cover_size = if column {
            SIDE_COLUMN - GUTTER - px(4.)
        } else if stacked {
            px(128.)
        } else {
            px(160.)
        };
        let cover = artwork(
            "podcast-artwork",
            page.artwork.as_deref(),
            Some(&page.title),
            cover_size,
            cx,
        )
        .shadow(art_shadow(cx));
        let title = page_title(page.title.clone()).when(column || stacked, |this| {
            this.text_size(size::TITLE).line_height(relative(1.15))
        });
        let title = if page.state == PodcastState::Pending {
            h_flex()
                .id("podcast-pending")
                .aria_label(SharedString::from(page.title.clone()))
                .test_support()
                .gap(px(12.))
                .child(Spinner::new().color(p.accent))
                .child(
                    v_flex().min_w_0().child(title).child(
                        div()
                            .text_color(p.muted)
                            .child("Waiting for the server to fetch this feed"),
                    ),
                )
                .into_any_element()
        } else {
            title.into_any_element()
        };
        let mut meta: Vec<String> = page.author.iter().cloned().collect();
        meta.push(if episodes == 1 {
            "1 episode".to_owned()
        } else {
            format!("{episodes} episodes")
        });
        let text = v_flex()
            .min_w_0()
            .child(title)
            .child(
                div()
                    .mt(px(6.))
                    .text_color(p.muted)
                    .font_weight(FontWeight::MEDIUM)
                    .child(meta.join(" · ")),
            )
            .child(
                h_flex()
                    .mt(px(14.))
                    .gap(px(4.))
                    .flex_wrap()
                    .child(
                        h_flex()
                            .h(px(32.))
                            .pl(px(10.))
                            .pr(px(14.))
                            .gap(px(6.))
                            .rounded(ROUND)
                            .bg(p.surface.opacity(0.7))
                            .text_size(size::SMALL)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.muted)
                            .child(
                                div()
                                    .text_color(p.accent)
                                    .child(icon(PodIcon::Check, px(16.))),
                            )
                            .child("Subscribed"),
                    )
                    .child(self.render_settings(page.device, cx))
                    .child(self.render_more(page, cx)),
            );

        let lines = if column { 8 } else { 2 };
        // A rough fit of the description in its clamped lines: GPUI can't
        // tell whether clamped text overflowed, and offering "More" for
        // text that already shows in full would be a dead end.
        let width = if column {
            SIDE_COLUMN - GUTTER
        } else {
            (self.pane_width - GUTTER * 2.).min(px(640.))
        };
        let fits =
            (page.description.chars().count() as f32) < f32::from(width) / 7.5 * lines as f32;
        let description = (!page.description.is_empty()).then(|| {
            v_flex()
                .gap(px(4.))
                .child(
                    div()
                        .id("podcast-description")
                        .max_w(px(640.))
                        .text_color(mix(p.ink, p.background, 0.2))
                        .when(column, |this| {
                            this.text_size(size::SMALL).line_height(relative(1.55))
                        })
                        .when(!self.expanded, |this| {
                            this.line_clamp(lines).text_ellipsis()
                        })
                        .child(page.description.clone()),
                )
                .when(!fits, |this| {
                    this.child(
                        div()
                            .id("podcast-more-about")
                            .text_size(size::SMALL)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.accent_ink)
                            .cursor_pointer()
                            .hover(|this| this.underline())
                            .child(if self.expanded {
                                "Show less"
                            } else {
                                "More about this show"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.expanded = !this.expanded;
                                this.list.remeasure_items(0..1);
                                cx.notify();
                            })),
                    )
                })
        });
        let notice = notice.map(|notice| {
            let failing = matches!(page.state, PodcastState::Failing { .. });
            div()
                .id("podcast-notice")
                .aria_label(SharedString::from(notice.clone()))
                .test_support()
                .px(px(16.))
                .py(px(12.))
                .rounded(px(12.))
                .border_1()
                .map(|this| {
                    if failing {
                        this.bg(mix(p.surface, p.danger, 0.1))
                            .border_color(p.danger.opacity(0.25))
                    } else {
                        this.bg(mix(p.surface, p.amber, 0.12))
                            .border_color(p.amber.opacity(0.35))
                    }
                })
                .child(notice)
        });

        if column || stacked {
            v_flex()
                .pl(GUTTER)
                .pr(if column { px(4.) } else { GUTTER })
                .pt(px(40.))
                .pb(px(24.))
                .gap(px(16.))
                .child(cover)
                .child(text)
                .children(description)
                .children(notice)
                .into_any_element()
        } else {
            v_flex()
                .px(GUTTER)
                .pt(px(40.))
                .pb(px(16.))
                .gap(px(20.))
                .child(h_flex().items_end().gap(px(28.)).child(cover).child(text))
                .children(description)
                .children(notice)
                .into_any_element()
        }
    }

    /// The "This device" popover (D2 annotation 2): `podcast_device`, which
    /// never syncs. There is no notification toggle, as desktop has no
    /// new-episode notifications (spec 7.6).
    fn render_settings(&self, device: DeviceSettings, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        Popover::new("podcast-settings-popover")
            .trigger(
                quiet_pill("podcast-settings", cx)
                    .h(px(32.))
                    .px(px(12.))
                    .icon(icon(PodIcon::Settings, px(16.)))
                    .label("This device"),
            )
            .content(move |_, _, cx| {
                // Read at render time, so the popover follows each click.
                let device = view
                    .read(cx)
                    .page
                    .as_ref()
                    .map_or(device, |page| page.device);
                render_settings_content(&view, device, cx)
            })
    }

    /// Unsubscribe: in the ⋮ menu while the feed is fine, and a button of
    /// its own when the feed fails or is gone, where leaving is the obvious
    /// next step.
    fn render_more(&self, page: &PodcastPage, cx: &mut Context<Self>) -> AnyElement {
        if matches!(
            page.state,
            PodcastState::Failing { .. } | PodcastState::Gone
        ) {
            return danger_pill("podcast-unsubscribe", cx)
                .h(px(32.))
                .label("Unsubscribe")
                .on_click(cx.listener(|this, _, window, cx| this.confirm_unsubscribe(window, cx)))
                .into_any_element();
        }
        let view = cx.entity().downgrade();
        Popover::new("podcast-more-popover")
            .trigger(icon_button("podcast-more", PodIcon::More, cx).tooltip("More"))
            .content(move |_, _, cx| {
                let view = view.clone();
                v_flex().min_w(px(190.)).child(
                    Button::new("podcast-unsubscribe")
                        .ghost()
                        .w_full()
                        .justify_start()
                        .label("Unsubscribe…")
                        .on_click(cx.listener(move |state, _, window, cx| {
                            state.dismiss(window, cx);
                            let _ =
                                view.update(cx, |this, cx| this.confirm_unsubscribe(window, cx));
                        })),
                )
            })
            .into_any_element()
    }

    fn render_empty(&self, page: &PodcastPage, cx: &mut Context<Self>) -> impl IntoElement {
        let text = if page.state == PodcastState::Pending {
            "Episodes show up once the server has fetched the feed."
        } else {
            "This feed has no episodes."
        };
        div()
            .id("podcast-empty")
            .test_support()
            .w_full()
            .py(px(56.))
            .text_center()
            .text_color(palette(cx).muted)
            .child(text)
    }

    fn render_row(&self, index: usize, columns: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.listed().nth(index).cloned() else {
            return div().into_any_element();
        };
        let id = row.id;
        let previous = index
            .checked_sub(1)
            .and_then(|previous| self.listed().nth(previous))
            .map(|episode| episode.id);
        let mut ring = Ring::new(row.position_ms, row.duration_ms, row.played);
        if let Some((playing_id, playing, position_ms)) = crate::player::now_playing(cx)
            && playing_id == id
        {
            ring = Ring::new(position_ms, row.duration_ms, row.played);
            ring.current = true;
            ring.playing = playing;
        }
        let mut state = progress_label(&row);
        if row.archived {
            state.push_str(" · Archived");
        }
        let view = cx.entity().downgrade();
        div()
            .w_full()
            .child(
                div()
                    .w_full()
                    .max_w(CONTENT_MAX)
                    .mx_auto()
                    .px(GUTTER - px(12.))
                    .child(episode_row::row(
                        Row {
                            id,
                            prefix: "podcast",
                            show: None,
                            title: row.title.clone(),
                            when: Some(row.day.clone()),
                            length: length_label(row.duration_ms, row.position_ms, row.played),
                            state_label: Some(state),
                            played: row.played,
                            archived: row.archived,
                            ring,
                            selected: self.selected == Some(id),
                            after_highlight: previous.is_some() && previous == self.selected,
                            first: index == 0,
                            columns,
                        },
                        cx.listener(move |this, _, window, cx| {
                            this.focus_handle.focus(window, cx);
                            this.select(id, cx);
                        }),
                        move |change, cx| {
                            let _ = view.update(cx, |this, cx| this.change(id, change, cx));
                        },
                        cx,
                    )),
            )
            .into_any_element()
    }

    /// The selected episode, as the inbox's detail pane shows it, with the
    /// played and archive buttons toggling as the rows' do.
    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.selected.and_then(|id| self.episode(id)).cloned() else {
            return div().id("episode-detail").size_full().into_any_element();
        };
        let Some(page) = self.page.clone() else {
            return div().into_any_element();
        };
        let id = row.id;
        self.load_notes(id, cx);

        let mut meta = vec![row.day.clone()];
        meta.extend(row.duration_ms.map(duration_label));
        meta.push(progress_label(&row));
        if row.archived {
            meta.push("Archived".to_owned());
        }
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
        let (played, archived) = (row.played, row.archived);
        let header = detail_header(
            &Detail {
                artwork: page.artwork.clone(),
                podcast_title: page.title.clone(),
                title: row.title.clone(),
                meta: meta.join(" · "),
            },
            None,
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
                    .icon(icon(
                        if played {
                            PodIcon::Undo
                        } else {
                            PodIcon::Check
                        },
                        px(18.),
                    ))
                    .label(if played {
                        "Mark unplayed"
                    } else {
                        "Mark played"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change(id, EpisodeChange::Played(!played), cx);
                    })),
            )
            .child(
                pill("detail-archive", cx)
                    .icon(icon(
                        if archived {
                            PodIcon::Unarchive
                        } else {
                            PodIcon::Archive
                        },
                        px(18.),
                    ))
                    .label(if archived { "Unarchive" } else { "Archive" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change(id, EpisodeChange::Archived(!archived), cx);
                    })),
            )
            .child(detail_control(&self.downloads, id, self.id, cx))
            .when_some(row.link.clone(), |this, link| {
                this.child(
                    quiet_pill("detail-link", cx)
                        .icon(icon(PodIcon::External, px(18.)))
                        .label("Episode page")
                        .on_click(move |_, _, cx| cx.open_url(&link)),
                )
            });
        let notes = match self.notes.as_ref().and_then(|notes| notes.parsed.as_ref()) {
            Some(notes) => div()
                .id("detail-notes")
                .test_support()
                .child(show_notes::render::render("show-notes", notes, cx))
                .into_any_element(),
            None => div()
                .id("detail-notes-loading")
                .test_support()
                .child(Spinner::new())
                .into_any_element(),
        };
        detail_pane(header, actions, notes, cx).into_any_element()
    }
}

/// A list pane narrower than this stacks the header.
const STACK_BELOW: Pixels = px(600.);

/// How far down the show's blurred cover washes the page before it fades.
const WASH_HEIGHT: Pixels = px(480.);

/// The popover's body: "keep newest N" as a stepper from Off to
/// [`AUTO_KEEP_MAX`], and whether auto downloads may use a metered network.
fn render_settings_content(
    view: &Entity<PodcastView>,
    device: DeviceSettings,
    cx: &App,
) -> AnyElement {
    let set = |settings: DeviceSettings| {
        let view = view.downgrade();
        move |_: &_, _: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |this, cx| this.set_device(settings, cx));
        }
    };
    let keep = SharedString::from(match device.auto_keep {
        0 => "Off".to_owned(),
        n => n.to_string(),
    });
    let fewer = DeviceSettings {
        auto_keep: device.auto_keep.saturating_sub(1),
        ..device
    };
    let more = DeviceSettings {
        auto_keep: (device.auto_keep + 1).min(AUTO_KEEP_MAX),
        ..device
    };
    let view = view.downgrade();
    let metered = move |checked: &bool, _: &mut Window, cx: &mut App| {
        let settings = DeviceSettings {
            auto_on_metered: *checked,
            ..device
        };
        let _ = view.update(cx, |this, cx| this.set_device(settings, cx));
    };
    v_flex()
        .id("podcast-settings-content")
        .test_support()
        .w(px(300.))
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("On this computer"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette(cx).muted)
                        .child("This device only"),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child("Auto-download"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(palette(cx).muted)
                                .child("Keep the newest unplayed episodes"),
                        ),
                )
                .child(
                    icon_button("auto-keep-fewer", PodIcon::Minus, cx)
                        .size(px(30.))
                        .disabled(device.auto_keep == 0)
                        .on_click(set(fewer)),
                )
                .child(
                    div()
                        .id("auto-keep")
                        .aria_label(keep.clone())
                        .test_support()
                        .w(px(28.))
                        .text_center()
                        .text_sm()
                        .child(keep),
                )
                .child(
                    icon_button("auto-keep-more", PodIcon::Plus, cx)
                        .size(px(30.))
                        .disabled(device.auto_keep == AUTO_KEEP_MAX)
                        .on_click(set(more)),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child("On metered networks"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(palette(cx).muted)
                                .child("Otherwise auto-downloads wait for an unmetered one"),
                        ),
                )
                .child(
                    Switch::new("auto-on-metered")
                        .checked(device.auto_on_metered)
                        .on_click(metered),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::{Result, WrapErr as _};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, TestAppContext};
    use rusqlite::{Transaction, params};

    use crate::app::tests::{TestApp, signed_in};
    use crate::shell::sidebar::podcast_element_id;

    const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
    const NEWEST: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c01";
    const MIDDLE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c02";
    const OLDEST: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c03";
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    const LONG_TITLE: &str = "EP 150: The one with an extremely long title that keeps going \
                              well past where any reasonable layout would want it to stop, and \
                              then some more, just to be sure it never fits on one line";

    /// A signed-in account subscribed to one podcast with `status`. Unless
    /// it is a placeholder, it has three episodes a day apart, the oldest
    /// played, and two downloads, one of them pinned. A failing one has
    /// answered HTTP 404 for three days.
    fn seed(status: &'static str) -> impl FnOnce(&Transaction<'_>) -> Result<()> + Send + 'static {
        move |transaction| {
            signed_in(transaction)?;
            let now = pod_protocol::Timestamp::now().as_epoch_ms();
            let (title, local_title, version) = if status == "pending" {
                (None, Some("Syntax"), 0)
            } else {
                (Some("Darknet Diaries"), None, 4)
            };
            let (failing_since, feed_error) = if status == "failing" {
                (
                    Some(now - 3 * DAY_MS),
                    Some(r#"{"code":"http","status":404}"#),
                )
            } else {
                (None, None)
            };
            transaction
                .execute(
                    "insert into podcast (id, feed_url, status, failing_since, feed_error, title,
                         local_title, author, version)
                     values (?1, 'https://example.com/feed', ?2, ?3, ?4, ?5, ?6, 'Jack Rhysider',
                             ?7)",
                    params![
                        PODCAST,
                        status,
                        failing_since,
                        feed_error,
                        title,
                        local_title,
                        version
                    ],
                )
                .context("insert the podcast")?;
            transaction
                .execute(
                    "insert into subscription
                         (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
                     values (?1, 'https://example.com/feed', 1, ?2, 'dev')",
                    params![PODCAST, now - 30 * DAY_MS],
                )
                .context("insert the subscription")?;
            if status == "pending" {
                return Ok(());
            }
            for (days_ago, id, title) in [
                (1, NEWEST, "EP 151: The pager"),
                (2, MIDDLE, LONG_TITLE),
                (3, OLDEST, "EP 149: Mini-stories"),
            ] {
                transaction
                    .execute(
                        "insert into episode (id, podcast_id, title, summary_html, content_html,
                             published_at, duration_ms, audio_url, audio_type, version)
                         values (?1, ?2, ?3, '', '<p>Notes for <b>' || ?3 || '</b></p>', ?4,
                                 3120000, 'https://example.com/a.mp3', 'audio/mpeg', 1)",
                        params![id, PODCAST, title, now - days_ago * DAY_MS],
                    )
                    .context("insert an episode")?;
            }
            transaction
                .execute_batch(&format!(
                    "insert into episode_state (episode_id, podcast_id, played)
                     values ('{OLDEST}', '{PODCAST}', 1);
                     insert into download (episode_id, state, audio_url, path)
                     values ('{NEWEST}', 'complete', 'a', 'p'), ('{MIDDLE}', 'queued', 'a', 'p');
                     insert into download_wish (episode_id, source, created_at)
                     values ('{MIDDLE}', 'pin', 0);"
                ))
                .context("insert the state and downloads")
        }
    }

    /// Clicks the podcast in the sidebar.
    fn open_podcast(app: &TestApp, cx: &mut TestAppContext) {
        app.wait_until(cx, "the sidebar lists the podcast", |window| {
            window
                .try_find(format!("podcast-unplayed-{PODCAST}"))
                .is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            window.within("sidebar").click(podcast_element_id(0), cx);
        })
        .expect("the window is still open");
    }

    fn progress(id: &str) -> String {
        format!("podcast-progress-{id}")
    }

    #[gpui_kit::test]
    fn a_pending_podcast_shows_a_spinner_and_its_directory_title(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("pending"));
        open_podcast(&app, cx);
        app.wait_for_label(cx, "podcast-pending", "Syntax");
        cx.update_window(app.window, |_, window, _| {
            assert!(window.try_find("podcast-notice").is_none());
            assert!(window.find("podcast-empty").visible());
            assert!(
                window.find("podcast-settings").visible(),
                "settings work at once"
            );
            assert!(
                window.find("podcast-more").visible(),
                "unsubscribing waits in the menu while the feed may still come"
            );
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn a_failing_podcast_shows_why_and_offers_to_unsubscribe(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("failing"));
        open_podcast(&app, cx);
        app.wait_for_label(
            cx,
            "podcast-notice",
            "The feed returned HTTP 404. Pod has kept trying for 3 days and will keep checking \
             for new episodes.",
        );
        cx.update_window(app.window, |_, window, _| {
            assert!(window.try_find("podcast-pending").is_none());
            assert!(window.find("podcast-unsubscribe").visible());
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn a_gone_podcast_says_its_episodes_and_downloads_are_kept(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("gone"));
        open_podcast(&app, cx);
        app.wait_for_label(
            cx,
            "podcast-notice",
            "The publisher removed this feed, and Pod has stopped checking for new episodes. \
             Your 3 episodes and 2 downloads are kept.",
        );
        // The header with its notice fills most of the test window, so the
        // newest row is the one in view.
        app.wait_until(cx, "the episodes list", |window| {
            window.try_find(format!("podcast-row-{NEWEST}")).is_some()
        });
        cx.update_window(app.window, |_, window, _| {
            assert!(
                window.find("podcast-unsubscribe").visible(),
                "a gone feed offers leaving outright"
            );
            assert!(window.find(format!("podcast-row-{NEWEST}")).visible());
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn an_ok_podcast_lists_every_episode_with_its_state(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("ok"));
        open_podcast(&app, cx);
        app.wait_for_label(cx, &progress(OLDEST), "Played");
        cx.update_window(app.window, |_, window, _| {
            assert_eq!(window.find("page-title").label(), Some("Darknet Diaries"));
            assert!(window.try_find("podcast-notice").is_none());
            let tops: Vec<_> = [NEWEST, MIDDLE, OLDEST]
                .map(|id| window.find(format!("podcast-row-{id}")).bounds().top())
                .into();
            assert!(tops.is_sorted(), "newest first: {tops:?}");
            assert_eq!(window.find(progress(NEWEST)).label(), Some("Unplayed"));
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn rows_span_the_list_and_keep_their_actions_in_one_column(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("ok"));
        open_podcast(&app, cx);
        app.wait_for_label(cx, &progress(OLDEST), "Played");
        cx.update_window(app.window, |_, window, _| {
            let list = window.find("podcast-list-column").bounds();
            let first = window.find(format!("podcast-row-{NEWEST}")).bounds();
            assert!(
                first.left() >= list.left() && first.right() <= list.right(),
                "the rows {first:?} sit inside the list {list:?}"
            );
            for id in [MIDDLE, OLDEST] {
                let row = window.find(format!("podcast-row-{id}")).bounds();
                assert_eq!(
                    (row.left(), row.right()),
                    (first.left(), first.right()),
                    "row {id} spans the list as the first does"
                );
            }
            let actions = [NEWEST, MIDDLE, OLDEST]
                .map(|id| window.find(format!("podcast-actions-{id}")).bounds());
            assert!(
                actions[1].right() <= list.right(),
                "the long title leaves its actions on screen: {actions:?}"
            );
            assert!(
                actions
                    .iter()
                    .all(|bounds| bounds.left() == actions[0].left()),
                "the actions line up: {actions:?}"
            );
        })
        .expect("the window is still open");
    }

    /// Renders until the selected episode's notes have loaded and take up
    /// room.
    fn wait_for_notes(app: &TestApp, cx: &mut TestAppContext) {
        app.wait_until(cx, "the show notes show", |window| {
            window
                .try_find("detail-notes")
                .is_some_and(|notes| notes.bounds().size.height > gpui_kit::px(0.))
        });
    }

    #[gpui_kit::test]
    fn the_selected_row_fills_the_detail_pane_which_acts_on_it(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("ok"));
        open_podcast(&app, cx);
        app.wait_for_label(cx, "detail-title", "EP 151: The pager");
        wait_for_notes(&app, cx);

        cx.update_window(app.window, |_, window, cx| {
            window.click(format!("podcast-row-{OLDEST}"), cx);
            assert_eq!(
                window.find("detail-title").label(),
                Some("EP 149: Mini-stories")
            );
        })
        .expect("the window is still open");
        wait_for_notes(&app, cx);
        cx.update_window(app.window, |_, window, cx| {
            window.click("detail-download", cx);
        })
        .expect("the window is still open");
        // The reconciler isn't running, so the pin waits for its row.
        app.wait_for_label(cx, "detail-download-status", "Queued");

        cx.update_window(app.window, |_, window, cx| {
            window.press("k", cx);
            assert_eq!(window.find("detail-title").label(), Some(LONG_TITLE));
            window.click("detail-played", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, &progress(MIDDLE), "Played");
        cx.update_window(app.window, |_, window, cx| {
            window.click("detail-archive", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, &progress(MIDDLE), "Played · Archived");
        assert_eq!(state(&app, MIDDLE), Some((true, true)));
        wait_for_notes(&app, cx);
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
    fn rows_toggle_played_and_archived_and_mark_all_played_takes_the_rest(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("ok"));
        open_podcast(&app, cx);
        app.wait_for_label(cx, &progress(OLDEST), "Played");

        cx.update_window(app.window, |_, window, cx| {
            // The row's buttons show while the pointer is over it.
            window.hover(format!("podcast-row-{OLDEST}"), cx);
            window.click(format!("row-played-{OLDEST}"), cx);
            window.hover(format!("podcast-row-{MIDDLE}"), cx);
            window.click(format!("row-archive-{MIDDLE}"), cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, &progress(OLDEST), "Unplayed");
        app.wait_for_label(cx, &progress(MIDDLE), "Unplayed · Archived");
        assert_eq!(state(&app, OLDEST), Some((false, false)));
        assert_eq!(state(&app, MIDDLE), Some((false, true)));

        cx.update_window(app.window, |_, window, cx| {
            window.click(format!("podcast-row-{NEWEST}"), cx);
            window.press("j", cx);
            window.press("e", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, &progress(MIDDLE), "Unplayed");

        cx.update_window(app.window, |_, window, cx| {
            window.click("mark-all-played", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "mark-all-played-title", "Mark 3 episodes played?");
        cx.update_window(app.window, |_, window, cx| {
            window.press("escape", cx);
        })
        .expect("the window is still open");
        app.wait_until(cx, "the dialog closes", |window| {
            window.try_find("mark-all-played-title").is_none()
        });
        assert_eq!(state(&app, NEWEST), None, "cancelling marks nothing played");

        cx.update_window(app.window, |_, window, cx| {
            window.click("mark-all-played", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "mark-all-played-title", "Mark 3 episodes played?");
        cx.update_window(app.window, |_, window, cx| {
            window.within("dialog").click("mark-all-played-confirm", cx);
        })
        .expect("the window is still open");
        for id in [NEWEST, MIDDLE, OLDEST] {
            app.wait_for_label(cx, &progress(id), "Played");
        }
        let queued: i64 = app.read(|connection| {
            connection
                .query_row(
                    "select count(*) from outbox where coalesce_key like 'played:%'",
                    [],
                    |row| row.get(0),
                )
                .context("count the queued played mutations")
        });
        assert_eq!(queued, 3);
    }

    #[gpui_kit::test]
    fn the_device_popover_writes_this_devices_settings(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("ok"));
        open_podcast(&app, cx);
        app.wait_for_label(cx, "page-title", "Darknet Diaries");
        cx.update_window(app.window, |_, window, cx| {
            window.click("podcast-settings", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "auto-keep", "Off");
        cx.update_window(app.window, |_, window, cx| {
            assert_eq!(window.find("auto-keep-fewer").disabled(), Some(true));
            window.click("auto-keep-more", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "auto-keep", "1");
        cx.update_window(app.window, |_, window, cx| {
            window.click("auto-keep-more", cx);
            window.click("auto-on-metered", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(cx, "auto-keep", "2");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let row: Option<(i64, i64, i64)> = app.read(|connection| {
                use rusqlite::OptionalExtension as _;
                connection
                    .query_row(
                        "select notify, auto_keep, auto_on_metered from podcast_device",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .context("read the device settings")
            });
            if row == Some((0, 2, 1)) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the settings land: {row:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let outbox: i64 = app.read(|connection| {
            connection
                .query_row("select count(*) from outbox", [], |row| row.get(0))
                .context("count the outbox")
        });
        assert_eq!(outbox, 0, "device settings never sync");
    }

    #[gpui_kit::test]
    fn unsubscribing_counts_the_downloads_and_leaves_the_sidebar_at_once(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("ok"));
        open_podcast(&app, cx);
        app.wait_for_label(cx, "page-title", "Darknet Diaries");
        cx.update_window(app.window, |_, window, cx| {
            window.click("podcast-more", cx);
        })
        .expect("the window is still open");
        app.wait_until(cx, "the menu opens", |window| {
            window.try_find("podcast-unsubscribe").is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            window.click("podcast-unsubscribe", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(
            cx,
            "unsubscribe-message",
            "2 downloads will be removed from this computer, including 1 you pinned. \
             Your listening history stays.",
        );
        cx.update_window(app.window, |_, window, cx| {
            window.within("dialog").click("unsubscribe-confirm", cx);
        })
        .expect("the window is still open");

        app.wait_until(cx, "the show leaves the sidebar", |window| {
            window
                .try_find(format!("podcast-unplayed-{PODCAST}"))
                .is_none()
        });
        app.wait_for_label(cx, "page-title", "Inbox");
        let (subscribed, queued): (bool, String) = app.read(|connection| {
            connection
                .query_row(
                    "select (select subscribed from subscription),
                            (select coalesce_key from outbox)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .context("read the subscription")
        });
        assert!(!subscribed);
        assert_eq!(queued, format!("sub:{PODCAST}"));
        assert!(
            app.sync.take_wanted().is_some(),
            "the unsubscribe reaches other devices with the next sync"
        );
    }
}
