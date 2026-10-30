//! The Podcasts page: every subscription as a cover with its title and
//! author, ending in an "Add a podcast" tile that goes to the search field.
//! It opens from the sidebar's "Podcasts" heading.
//!
//! Tiles keep a fixed width and the row fills with as many as fit, so the
//! grid uses any window width without capping it.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Role, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, px,
    relative,
};

use crate::app::{PodApp, Route};
use crate::inbox::SidebarPodcast;
use crate::search::view::FocusSearch;
use crate::shell::artwork::artwork;
use crate::shell::icon::PodIcon;
use crate::shell::layout::GUTTER;
use crate::shell::ui::{art_shadow, icon, page_title};
use crate::theme::{palette, size};

/// A tile's width: half the cached artwork's side, so covers stay sharp at
/// 2x scaling.
const TILE: f32 = 168.;

pub fn render(podcasts: &[SidebarPodcast], cx: &mut Context<PodApp>) -> AnyElement {
    let p = palette(cx);
    let tiles = podcasts.iter().map(|podcast| {
        let target = Route::Podcast(podcast.id);
        v_flex()
            .id(SharedString::from(format!("grid-podcast-{}", podcast.id)))
            .role(Role::Link)
            .aria_label(SharedString::from(podcast.title.clone()))
            .test_support()
            .w(px(TILE))
            .gap(px(2.))
            .cursor_pointer()
            .group("tile")
            .child(
                div()
                    .mb(px(8.))
                    .rounded(px(10.))
                    .shadow(art_shadow(cx))
                    .child(artwork(
                        SharedString::from(format!("grid-artwork-{}", podcast.id)),
                        podcast.artwork.as_deref(),
                        Some(&podcast.title),
                        px(TILE),
                        cx,
                    )),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(relative(1.3))
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(podcast.title.clone()),
            )
            .children(podcast.author.clone().map(|author| {
                div()
                    .text_size(size::SMALL)
                    .text_color(p.muted)
                    .truncate()
                    .child(author)
            }))
            .on_click(cx.listener(move |app, _, _, cx| app.navigate(target, cx)))
    });
    let add = v_flex()
        .id("grid-add-podcast")
        .role(Role::Button)
        .aria_label("Add a podcast")
        .test_support()
        .w(px(TILE))
        .gap(px(2.))
        .cursor_pointer()
        .group("add")
        .child(
            div()
                .mb(px(8.))
                .size(px(TILE))
                .rounded(px(10.))
                .border_2()
                .border_dashed()
                .border_color(p.faint)
                .text_color(p.muted)
                .flex()
                .items_center()
                .justify_center()
                .group_hover("add", |this| {
                    this.border_color(p.accent)
                        .text_color(p.accent)
                        .bg(p.accent_soft)
                })
                .child(icon(PodIcon::Plus, px(32.))),
        )
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Add a podcast"),
        )
        .child(
            div()
                .text_size(size::SMALL)
                .text_color(p.muted)
                .truncate()
                .child("Search the directory or paste a feed URL"),
        )
        .on_click(|_, window, cx| window.dispatch_action(Box::new(FocusSearch), cx));

    v_flex()
        .id("content")
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_y_scroll()
        .px(GUTTER)
        .pt(px(32.))
        .pb(px(48.))
        .gap(px(20.))
        .child(page_title("Podcasts"))
        .child(
            h_flex()
                .flex_wrap()
                .items_start()
                .gap_x(px(20.))
                .gap_y(px(28.))
                .children(tiles)
                .child(add),
        )
        .into_any_element()
}
