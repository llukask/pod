//! The podcast page (wireframes D2 and D7): a header with the feed's state
//! and actions, this device's settings in a popover, every episode newest
//! first, and the selected episode's detail pane with its show notes.
//!
//! Rows come from a live query, so they follow every write. Unlike the
//! inbox, rows stay when marked played or archived; their state shows.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use jiff::tz::TimeZone;
use pod_protocol::{EpisodeId, PodcastId, Timestamp};

use super::{
    AUTO_KEEP_MAX, DeviceSettings, PodcastEpisode, PodcastPage, PodcastState, load_page,
    load_show_notes, progress_label, status_notice, unsubscribe_message,
};
use crate::downloads::view::{DownloadList, detail_control};
use crate::inbox::view::{Archive, MarkPlayed, Play, SelectNext, SelectPrevious};
use crate::inbox::{duration_label, reselect, step_selection, time_left_label};
use crate::runtime::Background;
use crate::shell::artwork::artwork;
use crate::shell::episode_row;
use crate::show_notes::{self, ShowNotes};
use crate::store::{Store, live};
use crate::sync::SyncHandle;
use crate::sync::local_write::{self, EpisodeChange, LocalChange};

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
    scroll: UniformListScrollHandle,
    _query: Task<()>,
    _downloads_changed: Subscription,
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
        Self {
            id,
            focus_handle,
            _downloads_changed: cx.observe(&downloads, |_, _, cx| cx.notify()),
            downloads,
            page: None,
            selected: None,
            notes: None,
            scroll: UniformListScrollHandle::new(),
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
        let ids = |page: &Option<PodcastPage>| -> Vec<EpisodeId> {
            page.iter()
                .flat_map(|page| &page.episodes)
                .map(|episode| episode.id)
                .collect()
        };
        let before = ids(&self.page);
        self.page = page;
        self.selected = reselect(&before, &ids(&self.page), self.selected);
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
        if let Some(index) = self.episodes().iter().position(|row| row.id == id) {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let ids: Vec<EpisodeId> = self.episodes().iter().map(|row| row.id).collect();
        if let Some(id) = step_selection(&ids, self.selected, step) {
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

    fn mark_all_played(&mut self, cx: &mut Context<Self>) {
        let store = cx.global::<Store>().clone();
        let sync = cx.global::<SyncHandle>().clone();
        let id = self.id;
        cx.spawn(async move |_, _| {
            if let Err(error) = super::mark_all_played(&store, &sync, id).await {
                tracing::error!("marking podcast {id} played failed: {error:#}");
            }
        })
        .detach();
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
                .title(title.clone())
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
                        .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                        .child(
                            DialogAction::new().child(
                                Button::new("unsubscribe-confirm")
                                    .danger()
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
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
        let header = self.render_header(&page, cx);
        if page.episodes.is_empty() {
            return root.child(header).child(self.render_list(&page, cx));
        }
        // The same split as the inbox: a fixed list column, the detail
        // pane in the rest.
        root.child(header).child(
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    v_flex()
                        .id("podcast-list-column")
                        .test_support()
                        .flex_none()
                        .w(px(380.))
                        .h_full()
                        .child(self.render_list(&page, cx)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .child(self.render_detail(cx)),
                ),
        )
    }
}

impl PodcastView {
    fn render_header(&self, page: &PodcastPage, cx: &mut Context<Self>) -> impl IntoElement {
        let episodes = u32::try_from(page.episodes.len()).unwrap_or(u32::MAX);
        let notice = status_notice(&page.state, episodes, page.downloads.total);
        let title = div()
            .id("page-title")
            .aria_label(SharedString::from(page.title.clone()))
            .test_support()
            .text_xl()
            .font_weight(FontWeight::SEMIBOLD)
            .truncate()
            .child(page.title.clone());
        let headline = if page.state == PodcastState::Pending {
            h_flex()
                .id("podcast-pending")
                .aria_label(SharedString::from(page.title.clone()))
                .test_support()
                .gap_3()
                .child(Spinner::new())
                .child(
                    v_flex().min_w_0().child(title).child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Waiting for the server to fetch this feed"),
                    ),
                )
                .into_any_element()
        } else {
            let mut meta: Vec<String> = page.author.iter().cloned().collect();
            meta.push(if episodes == 1 {
                "1 episode".to_owned()
            } else {
                format!("{episodes} episodes")
            });
            v_flex()
                .min_w_0()
                .child(title)
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(meta.join(" · ")),
                )
                .into_any_element()
        };
        let unsubscribe = Button::new("podcast-unsubscribe")
            .label("Unsubscribe")
            .small()
            .on_click(cx.listener(|this, _, window, cx| this.confirm_unsubscribe(window, cx)));
        let any_unplayed = page.episodes.iter().any(|episode| !episode.played);

        v_flex()
            .px_5()
            .py_4()
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_4()
                    .child(artwork(
                        "podcast-artwork",
                        page.artwork.as_deref(),
                        px(64.),
                        cx,
                    ))
                    .child(div().flex_1().min_w_0().child(headline)),
            )
            .when_some(notice, |this, notice| {
                this.child(
                    div()
                        .id("podcast-notice")
                        .aria_label(SharedString::from(notice.clone()))
                        .test_support()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().warning)
                        .text_sm()
                        .child(notice),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(self.render_settings(page.device, cx))
                    .child(
                        Button::new("mark-all-played")
                            .outline()
                            .small()
                            .icon(IconName::Check)
                            .label("Mark all played")
                            .disabled(!any_unplayed)
                            .on_click(cx.listener(|this, _, _, cx| this.mark_all_played(cx))),
                    )
                    // Failing and gone feeds make leaving the obvious next
                    // step; an ok feed keeps it quieter.
                    // TODO: move it into an overflow menu for ok feeds, as
                    // D2 draws it.
                    .child(
                        if matches!(
                            page.state,
                            PodcastState::Failing { .. } | PodcastState::Gone { .. }
                        ) {
                            unsubscribe.danger()
                        } else {
                            unsubscribe.ghost()
                        },
                    ),
            )
    }

    /// The "This device" popover (D2 annotation 2): `podcast_device`, which
    /// never syncs. There is no notification toggle, as desktop has no
    /// new-episode notifications (spec 7.6).
    fn render_settings(&self, device: DeviceSettings, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        Popover::new("podcast-settings-popover")
            .trigger(
                Button::new("podcast-settings")
                    .outline()
                    .small()
                    .icon(IconName::Settings)
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

    fn render_list(&self, page: &PodcastPage, cx: &mut Context<Self>) -> AnyElement {
        if page.episodes.is_empty() {
            let text = if page.state == PodcastState::Pending {
                "Episodes show up once the server has fetched the feed."
            } else {
                "This feed has no episodes."
            };
            return div()
                .id("podcast-empty")
                .test_support()
                .p_5()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element();
        }
        uniform_list(
            "podcast-list",
            page.episodes.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let rows: Vec<PodcastEpisode> = this.episodes()[range].to_vec();
                rows.iter()
                    .map(|row| this.render_row(row, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .into_any_element()
    }

    fn render_row(&self, row: &PodcastEpisode, cx: &mut Context<Self>) -> ListItem {
        let id = row.id;
        let mut meta = vec![row.day.clone()];
        meta.extend(row.duration_ms.map(duration_label));
        let mut progress = progress_label(row);
        if row.archived {
            progress.push_str(" · Archived");
        }
        let progress = SharedString::from(progress);
        let muted = cx.theme().muted_foreground;
        // A ListItem is only as wide as its content, and its inner wrapper
        // can't shrink below the content's min-content width, which for a
        // truncated title is the whole title. So the row takes the list's
        // width, and the title column starts from zero width with its
        // overflow clipped; `min_w_0` alone still let a long title push the
        // actions out. The state sits in the meta line, so the actions are
        // the only right-hand column.
        ListItem::new(SharedString::from(format!("podcast-row-{id}")))
            .w_full()
            .h(px(56.))
            .px_5()
            .selected(self.selected == Some(id))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_handle.focus(window, cx);
                this.select(id, cx);
            }))
            .child(
                h_flex().w_full().overflow_hidden().child(
                    v_flex()
                        .w_0()
                        .flex_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .when(row.played, |this| this.text_color(muted))
                                .child(row.title.clone()),
                        )
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_1()
                                .text_xs()
                                .text_color(muted)
                                .child(div().flex_none().child(meta.join(" · ")))
                                .child(div().flex_none().child("·"))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("podcast-progress-{id}")))
                                        .aria_label(progress.clone())
                                        .test_support()
                                        .min_w_0()
                                        .truncate()
                                        .child(progress),
                                ),
                        ),
                ),
            )
            .suffix({
                let view = cx.entity().downgrade();
                let (played, archived) = (row.played, row.archived);
                move |_, _| {
                    let view = view.clone();
                    h_flex()
                        .id(SharedString::from(format!("podcast-actions-{id}")))
                        .test_support()
                        .flex_none()
                        .gap_1()
                        .child(
                            Button::new(SharedString::from(format!("row-play-{id}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Play)
                                .tooltip("Play (enter)")
                                .on_click(move |_, _, cx| crate::player::play(id, cx)),
                        )
                        .child(episode_row::actions(
                            id,
                            played,
                            archived,
                            move |change, cx| {
                                let _ = view.update(cx, |this, cx| this.change(id, change, cx));
                            },
                        ))
                }
            })
    }

    /// The selected episode, as the inbox's detail pane shows it, with the
    /// played and archive buttons toggling as the rows' do.
    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.selected.and_then(|id| self.episode(id)).cloned() else {
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
        let id = row.id;
        self.load_notes(id, cx);

        let mut meta = vec![row.day.clone()];
        meta.extend(row.duration_ms.map(duration_label));
        meta.push(progress_label(&row));
        if row.archived {
            meta.push("Archived".to_owned());
        }
        let play_label = match time_left_label(row.duration_ms, row.position_ms) {
            Some(left) => format!("Resume · {left}"),
            None => "Play".to_owned(),
        };
        let (played, archived) = (row.played, row.archived);
        let actions = h_flex()
            .gap_2()
            .child(
                Button::new("detail-play")
                    .primary()
                    .icon(IconName::Play)
                    .label(play_label)
                    .on_click(move |_, _, cx| crate::player::play(id, cx)),
            )
            .child(
                Button::new("detail-played")
                    .outline()
                    .icon(IconName::Check)
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
                Button::new("detail-archive")
                    .outline()
                    .label(if archived { "Unarchive" } else { "Archive" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change(id, EpisodeChange::Archived(!archived), cx);
                    })),
            );
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
        v_flex()
            .id("episode-detail")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .id("detail-title")
                            .aria_label(SharedString::from(row.title.clone()))
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
            )
            .child(actions)
            .child(detail_control(&self.downloads, id, self.id, cx))
            .child(div().h(px(1.)).bg(cx.theme().border))
            .child(notes)
            .into_any_element()
    }
}

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
                        .text_color(cx.theme().muted_foreground)
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
                                .text_color(cx.theme().muted_foreground)
                                .child("Keep the newest unplayed episodes"),
                        ),
                )
                .child(
                    Button::new("auto-keep-fewer")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Minus)
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
                    Button::new("auto-keep-more")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
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
                                .text_color(cx.theme().muted_foreground)
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
    /// played, and two downloads, one of them pinned.
    fn seed(
        status: &'static str,
        message: Option<&'static str>,
    ) -> impl FnOnce(&Transaction<'_>) -> Result<()> + Send + 'static {
        move |transaction| {
            signed_in(transaction)?;
            let now = pod_protocol::Timestamp::now().as_epoch_ms();
            let (title, local_title, version) = if status == "pending" {
                (None, Some("Syntax"), 0)
            } else {
                (Some("Darknet Diaries"), None, 4)
            };
            transaction
                .execute(
                    "insert into podcast (id, feed_url, status, status_message, title,
                         local_title, author, version)
                     values (?1, 'https://example.com/feed', ?2, ?3, ?4, ?5, 'Jack Rhysider', ?6)",
                    params![PODCAST, status, message, title, local_title, version],
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
        let app = TestApp::open(cx, seed("pending", None));
        open_podcast(&app, cx);
        app.wait_for_label(cx, "podcast-pending", "Syntax");
        cx.update_window(app.window, |_, window, _| {
            assert!(window.try_find("podcast-notice").is_none());
            assert!(window.find("podcast-empty").visible());
            assert!(
                window.find("podcast-settings").visible(),
                "settings work at once"
            );
            assert!(window.find("podcast-unsubscribe").visible());
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn a_failing_podcast_shows_why_and_offers_to_unsubscribe(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("failing", Some("Feed returned HTTP 404")));
        open_podcast(&app, cx);
        app.wait_for_label(
            cx,
            "podcast-notice",
            "Feed returned HTTP 404. Pod keeps checking for new episodes.",
        );
        cx.update_window(app.window, |_, window, _| {
            assert!(window.try_find("podcast-pending").is_none());
            assert!(window.find("podcast-unsubscribe").visible());
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn a_gone_podcast_says_its_episodes_and_downloads_are_kept(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("gone", Some("Feed returned HTTP 410")));
        open_podcast(&app, cx);
        app.wait_for_label(
            cx,
            "podcast-notice",
            "Feed returned HTTP 410. The publisher removed this feed, and Pod has stopped \
             checking for new episodes. Your 3 episodes and 2 downloads are kept.",
        );
        cx.update_window(app.window, |_, window, _| {
            assert!(window.find("podcast-unsubscribe").visible());
            assert!(window.find(format!("podcast-row-{OLDEST}")).visible());
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn an_ok_podcast_lists_every_episode_with_its_state(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, seed("ok", None));
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
        let app = TestApp::open(cx, seed("ok", None));
        open_podcast(&app, cx);
        app.wait_for_label(cx, &progress(OLDEST), "Played");
        cx.update_window(app.window, |_, window, _| {
            let list = window.find("podcast-list-column").bounds();
            for id in [NEWEST, MIDDLE, OLDEST] {
                let row = window.find(format!("podcast-row-{id}")).bounds();
                assert_eq!(
                    (row.left(), row.right()),
                    (list.left(), list.right()),
                    "row {id} spans the list"
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
        let app = TestApp::open(cx, seed("ok", None));
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
        let app = TestApp::open(cx, seed("ok", None));
        open_podcast(&app, cx);
        app.wait_for_label(cx, &progress(OLDEST), "Played");

        cx.update_window(app.window, |_, window, cx| {
            window.click(format!("row-played-{OLDEST}"), cx);
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
        let app = TestApp::open(cx, seed("ok", None));
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
        let app = TestApp::open(cx, seed("ok", None));
        open_podcast(&app, cx);
        app.wait_for_label(cx, "page-title", "Darknet Diaries");
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
