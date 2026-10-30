//! An episode as the inbox and a show's page list it, and the detail pane
//! beside the list, shared by both pages.
//!
//! A row: [cover, inbox only] [show name / title, up to two lines / "Oct 21
//! · 43 min"] [mark played and archive, on hover] [the play button with its
//! progress ring]. A list wide enough shows the date and length as aligned
//! columns instead of the meta line, like a track list.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px, relative,
};
use pod_protocol::EpisodeId;

use super::artwork::artwork;
use super::icon::PodIcon;
use super::ui::{Observed, Ring, art_shadow, icon_button, play_button, tabular};
use crate::sync::local_write::EpisodeChange;
use crate::theme::{FONT_DISPLAY, mix, palette, size};

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// What a row shows.
#[derive(Debug, Clone)]
pub struct Row {
    pub id: EpisodeId,
    /// The page's prefix of the row's element ids: `<prefix>-row-<id>`.
    pub prefix: &'static str,
    /// The show's name and cover, which the inbox shows and a show's own
    /// page leaves out.
    pub show: Option<(String, Option<String>)>,
    pub title: String,
    /// The date, or `None` where the group heading says it.
    pub when: Option<String>,
    /// "43 min", "21 min left", "Played".
    pub length: Option<String>,
    /// For screen readers and tests, the episode's state in words.
    pub state_label: Option<String>,
    pub played: bool,
    pub archived: bool,
    pub ring: Ring,
    pub selected: bool,
    /// The row under a highlighted one draws no hairline above it.
    pub after_highlight: bool,
    pub first: bool,
    pub columns: bool,
}

/// The episode's length or how much is left, as a row shows it.
pub fn length_label(duration_ms: Option<i64>, position_ms: i64, played: bool) -> Option<String> {
    if played {
        return Some("Played".to_owned());
    }
    crate::inbox::time_left_label(duration_ms, position_ms)
        .or_else(|| duration_ms.map(crate::inbox::duration_label))
}

/// The row. `on_select` opens it in the detail pane, `on_change` gets the
/// hover buttons' played and archive changes, and the play button plays or
/// pauses it.
pub fn row(
    row: Row,
    on_select: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_change: impl Fn(EpisodeChange, &mut App) + Clone + 'static,
    cx: &App,
) -> Observed<Stateful<Div>> {
    let p = palette(cx);
    let id = row.id;
    let group = SharedString::from(format!("{}-row-{id}", row.prefix));
    let highlighted = row.selected;
    let title_color = if row.ring.current {
        p.accent_ink
    } else if row.played {
        p.muted
    } else {
        p.ink
    };
    let meta: Vec<String> = row
        .when
        .iter()
        .cloned()
        .chain(row.length.iter().cloned())
        .chain(row.archived.then(|| "Archived".to_owned()))
        .collect();
    let state_label = SharedString::from(row.state_label.clone().unwrap_or_default());
    let state_id = SharedString::from(format!("{}-progress-{id}", row.prefix));

    let text = v_flex()
        .flex_1()
        .min_w_0()
        .gap(px(1.))
        .when_some(row.show.as_ref(), |this, (show, _)| {
            this.child(
                div()
                    .text_size(size::SMALL)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted)
                    .truncate()
                    .child(show.clone()),
            )
        })
        .child(
            div()
                .id(SharedString::from(format!("{}-row-title-{id}", row.prefix)))
                .test_support()
                .font_weight(if row.played {
                    FontWeight::NORMAL
                } else {
                    FontWeight::SEMIBOLD
                })
                .line_height(relative(1.3))
                .text_color(title_color)
                .line_clamp(2)
                .text_ellipsis()
                .child(row.title.clone()),
        )
        .when(!row.columns, |this| {
            this.child(
                div()
                    .id(state_id.clone())
                    .aria_label(state_label.clone())
                    .test_support()
                    .text_size(size::SMALL)
                    .text_color(p.muted)
                    .font_features(tabular())
                    .truncate()
                    .child(meta.join(" · ")),
            )
        });

    let column = |width: f32| {
        div()
            .flex_none()
            .w(px(width))
            .text_size(size::SMALL)
            .text_color(p.muted)
            .font_features(tabular())
            .whitespace_nowrap()
    };
    let link = h_flex()
        .id(SharedString::from(format!("{}-row-link-{id}", row.prefix)))
        .flex_1()
        .min_w_0()
        .gap(px(14.))
        .p(px(12.))
        .cursor_pointer()
        .when_some(row.show.as_ref(), |this, (show, file)| {
            this.child(
                artwork(
                    SharedString::from(format!("{}-artwork-{id}", row.prefix)),
                    file.as_deref(),
                    Some(show),
                    px(52.),
                    cx,
                )
                .border_1()
                .border_color(gpui_kit::black().opacity(0.06)),
            )
        })
        .child(text)
        .when(row.columns, |this| {
            let mut length: Vec<String> = row.length.iter().cloned().collect();
            if row.archived {
                length.push("Archived".to_owned());
            }
            this.child(column(104.).child(row.when.clone().unwrap_or_default()))
                .child(
                    column(136.)
                        .id(state_id.clone())
                        .aria_label(state_label.clone())
                        .test_support()
                        .child(length.join(" · ")),
                )
        })
        .on_click(on_select);

    let on_archive = on_change.clone();
    let (played, archived) = (row.played, row.archived);
    // Shown on the hovered and the selected row only. Hidden, not
    // transparent: a transparent button still takes clicks.
    let hover_actions = h_flex()
        .flex_none()
        .when(!highlighted, |this| this.invisible())
        .group_hover(group.clone(), |this| this.visible())
        .child(
            icon_button(
                SharedString::from(format!("row-played-{id}")),
                if played {
                    PodIcon::Undo
                } else {
                    PodIcon::Check
                },
                cx,
            )
            .tooltip(if played {
                "Mark unplayed (p)"
            } else {
                "Mark played (p)"
            })
            .on_click(move |_, _, cx| on_change(EpisodeChange::Played(!played), cx)),
        )
        .child(
            icon_button(
                SharedString::from(format!("row-archive-{id}")),
                if archived {
                    PodIcon::Unarchive
                } else {
                    PodIcon::Archive
                },
                cx,
            )
            .tooltip(if archived {
                "Unarchive (e)"
            } else {
                "Archive (e)"
            })
            .on_click(move |_, _, cx| on_archive(EpisodeChange::Archived(!archived), cx)),
        );

    h_flex()
        .id(group.clone())
        .test_support()
        .group(group.clone())
        .relative()
        .w_full()
        .gap(px(4.))
        .pr(px(10.))
        .rounded(px(12.))
        .map(|this| {
            if highlighted {
                this.bg(p.accent_soft)
            } else {
                this.hover(|this| this.bg(p.hover))
            }
        })
        // The hairline between rows, inset from the rounded corners, and
        // left out beside a highlighted row.
        .when(!row.first && !highlighted && !row.after_highlight, |this| {
            this.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(12.))
                    .right(px(12.))
                    .h(px(1.))
                    .bg(p.line),
            )
        })
        .child(link)
        .map(|this| {
            if row.columns {
                this.child(hover_actions)
            } else {
                // A narrow list can't spare their width beside the title,
                // so they float over its end, on a fade of the row's
                // colour, while the row is hovered or selected.
                let backdrop = if highlighted {
                    p.accent_soft
                } else {
                    mix(p.background, p.ink, if p.is_dark { 0.06 } else { 0.05 })
                };
                this.child(
                    h_flex()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(56.))
                        .pl(px(28.))
                        .when(!highlighted, |this| this.invisible())
                        .group_hover(group.clone(), |this| this.visible())
                        .bg(gpui_kit::linear_gradient(
                            90.,
                            gpui_kit::linear_color_stop(backdrop.opacity(0.), 0.),
                            gpui_kit::linear_color_stop(backdrop, 0.35),
                        ))
                        .child(hover_actions),
                )
            }
        })
        .child(
            h_flex()
                .id(SharedString::from(format!("{}-actions-{id}", row.prefix)))
                .test_support()
                .flex_none()
                .child(play_button(
                    SharedString::from(format!("row-play-{id}")),
                    row.ring,
                    px(42.),
                    &row.title,
                    move |_, _, cx| crate::player::play_or_pause(id, cx),
                    cx,
                )),
        )
}

// ==============================================================================
// The detail pane
// ==============================================================================

/// What the detail pane shows about an episode.
#[derive(Debug, Clone)]
pub struct Detail {
    pub artwork: Option<String>,
    pub podcast_title: String,
    pub title: String,
    pub meta: String,
}

/// The pane's header: the cover, the show's name as a link, the title and
/// its meta line. `on_show` opens the show's page, `None` on the show's own
/// page.
pub fn detail_header(
    detail: &Detail,
    on_show: Option<Handler>,
    cx: &App,
) -> impl IntoElement + use<> {
    let p = palette(cx);
    h_flex()
        .items_end()
        .gap(px(20.))
        .pr(px(32.))
        .child(
            artwork(
                "detail-artwork",
                detail.artwork.as_deref(),
                Some(&detail.podcast_title),
                px(112.),
                cx,
            )
            .shadow(art_shadow(cx)),
        )
        .child(
            v_flex()
                .min_w_0()
                .child(
                    div()
                        .id("detail-show")
                        .text_size(size::SMALL)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.accent_ink)
                        .when(on_show.is_some(), |this| {
                            this.cursor_pointer().hover(|this| this.underline())
                        })
                        .child(detail.podcast_title.clone())
                        .when_some(on_show, |this, on_show| this.on_click(on_show)),
                )
                .child(
                    div()
                        .id("detail-title")
                        .aria_label(SharedString::from(detail.title.clone()))
                        .test_support()
                        .mt(px(4.))
                        .mb(px(6.))
                        .font_family(FONT_DISPLAY)
                        .text_size(size::TITLE)
                        .font_weight(FontWeight::BOLD)
                        .line_height(relative(1.18))
                        .child(detail.title.clone()),
                )
                .child(
                    div()
                        .text_size(size::SMALL)
                        .text_color(p.muted)
                        .font_features(tabular())
                        .child(detail.meta.clone()),
                ),
        )
}

/// The pane around a header, its actions and the show notes: a surface
/// beside the list, the notes at a reading width.
pub fn detail_pane(
    header: impl IntoElement,
    actions: impl IntoElement,
    notes: AnyElement,
    cx: &App,
) -> Stateful<Div> {
    let p = palette(cx);
    v_flex()
        .id("episode-detail")
        .size_full()
        .overflow_y_scroll()
        .bg(p.surface)
        .border_l_1()
        .border_color(p.line)
        .child(
            v_flex()
                .max_w(px(860.))
                .px(super::layout::GUTTER)
                .pt(px(36.))
                .pb(px(64.))
                .child(header)
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .mt(px(24.))
                        .mb(px(28.))
                        .child(actions),
                )
                .child(
                    div()
                        .max_w(px(640.))
                        .pt(px(24.))
                        .border_t_1()
                        .border_color(p.line)
                        .text_color(mix(p.ink, p.background, 0.12))
                        .child(notes),
                ),
        )
}
