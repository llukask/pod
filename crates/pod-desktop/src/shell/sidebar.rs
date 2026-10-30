//! The sidebar (D1 annotation 2): the search field, the fixed pages, then
//! "Podcasts", which opens the covers grid, above each subscription with
//! its cover and unplayed count.

use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, div, px,
};

use pod_protocol::{PodcastId, PodcastStatus};

use super::icon::PodIcon;
use super::layout::SIDEBAR;
use super::ui::{Observed, icon, soft_shadow, tabular};
use crate::app::{PodApp, Route};
use crate::inbox::SidebarPodcast;
use crate::search::view::SearchView;
use crate::shell::artwork::artwork;
use crate::theme::{palette, size};

/// The fixed pages, top to bottom.
const PAGES: [(&str, PodIcon, Route); 3] = [
    ("Inbox", PodIcon::Inbox, Route::Inbox),
    ("Downloads", PodIcon::Download, Route::Downloads),
    ("Settings", PodIcon::Settings, Route::Settings),
];

pub fn render(
    current: Route,
    podcasts: &[SidebarPodcast],
    search: &Entity<SearchView>,
    cx: &mut Context<PodApp>,
) -> impl IntoElement {
    let p = palette(cx);
    // Every inbox episode belongs to a subscribed podcast, so the counts
    // add up to the inbox's.
    let inbox_count: usize = podcasts.iter().map(|podcast| podcast.unplayed).sum();
    let pages = PAGES.map(|(label, glyph, target)| {
        let active = current == target;
        nav_item(page_element_id(target), active, cx)
            .gap(px(12.))
            .h(px(38.))
            .font_weight(FontWeight::MEDIUM)
            .child(
                div()
                    .text_color(if active { p.accent } else { p.muted })
                    .child(icon(glyph, px(20.))),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .when(target == Route::Inbox, |this| {
                this.child(count_badge("inbox-count", inbox_count, cx))
            })
            .on_click(cx.listener(move |app, _, _, cx| app.navigate(target, cx)))
    });
    let heading_active = current == Route::Podcasts;
    let heading = div()
        .id("nav-podcasts")
        .role(Role::Button)
        .aria_label("Podcasts")
        .aria_selected(heading_active)
        .test_support()
        .mt(px(22.))
        .mb(px(6.))
        .px(px(10.))
        .py(px(2.))
        .rounded(px(6.))
        .text_size(size::SMALL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted)
        .cursor_pointer()
        .hover(|this| this.bg(p.hover).text_color(p.ink))
        .when(heading_active, |this| this.bg(p.hover).text_color(p.ink))
        .child("Podcasts")
        .on_click(cx.listener(|app, _, _, cx| app.navigate(Route::Podcasts, cx)));
    let shows = podcasts.iter().enumerate().map(|(index, podcast)| {
        let target = Route::Podcast(podcast.id);
        let active = current == target;
        let badge_id = SharedString::from(format!("podcast-unplayed-{}", podcast.id));
        nav_item(podcast_element_id(index), active, cx)
            .role(Role::TreeItem)
            .aria_label(SharedString::from(podcast.title.clone()))
            .h(px(42.))
            .gap(px(12.))
            .child(artwork(
                SharedString::from(format!("podcast-artwork-{}", podcast.id)),
                podcast.artwork.as_deref(),
                Some(&podcast.title),
                px(28.),
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(podcast.title.clone()),
            )
            .children(feed_marker(podcast.id, podcast.status, cx))
            .child(count_badge(badge_id, podcast.unplayed, cx))
            .on_click(cx.listener(move |app, _, _, cx| app.navigate(target, cx)))
    });

    v_flex()
        .id("sidebar")
        .test_support()
        .flex_none()
        .w(SIDEBAR)
        .h_full()
        .bg(p.sidebar)
        .child(
            div()
                .flex_none()
                .px(px(12.))
                .pt(px(6.))
                .pb(px(14.))
                .child(search.read(cx).render_field(cx)),
        )
        .child(
            v_flex()
                .id("sidebar-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(12.))
                .pb(px(24.))
                .gap(px(1.))
                .children(pages)
                .child(heading)
                .children(shows)
                .when(podcasts.is_empty(), |this| {
                    this.child(
                        div()
                            .id("podcasts-empty")
                            .test_support()
                            .px(px(10.))
                            .text_size(size::SMALL)
                            .text_color(p.faint)
                            .child("No podcasts yet"),
                    )
                }),
        )
}

/// A row of the sidebar: hover-tinted, and lifted onto the surface while
/// its page is open.
fn nav_item(id: String, active: bool, cx: &App) -> Observed<gpui_kit::Stateful<gpui_kit::Div>> {
    let p = palette(cx);
    h_flex()
        .id(SharedString::from(id))
        .aria_selected(active)
        .test_support()
        .w_full()
        .flex_none()
        .px(px(10.))
        .rounded(px(8.))
        .overflow_x_hidden()
        .cursor_pointer()
        .map(|this| {
            if active {
                this.bg(p.surface)
                    .shadow(vec![soft_shadow(p, 0.06, px(1.), px(2.))])
            } else {
                this.hover(|this| this.bg(p.hover))
            }
        })
}

/// A count after a sidebar label, or nothing for zero.
fn count_badge(id: impl Into<SharedString>, count: usize, cx: &App) -> AnyElement {
    let label = SharedString::from(if count == 0 {
        String::new()
    } else {
        count.to_string()
    });
    div()
        .id(id.into())
        .aria_label(label.clone())
        .test_support()
        .flex_none()
        .text_size(px(12.))
        .font_features(tabular())
        .text_color(palette(cx).faint)
        .child(label)
        .into_any_element()
}

/// The marker next to a podcast the server can't fetch (D1 annotation 2):
/// a failing feed and a gone one both get the warning sign, told apart by
/// the label. A pending placeholder shows nothing here; its page has the
/// spinner.
fn feed_marker(id: PodcastId, status: PodcastStatus, cx: &App) -> Option<AnyElement> {
    let label = match status {
        PodcastStatus::Pending | PodcastStatus::Ok => return None,
        PodcastStatus::Failing => "Feed can't be fetched",
        PodcastStatus::Gone => "Feed removed by the publisher",
    };
    Some(
        div()
            .id(SharedString::from(format!("podcast-status-{id}")))
            .aria_label(SharedString::from(label))
            .test_support()
            .flex_none()
            .text_color(palette(cx).amber)
            .child(Icon::new(IconName::TriangleAlert).xsmall())
            .into_any_element(),
    )
}

/// The element id of the sidebar item for a fixed page.
pub fn page_element_id(route: Route) -> String {
    let name = match route {
        Route::Inbox => "inbox",
        Route::Downloads => "downloads",
        Route::Settings => "settings",
        Route::Podcasts => "podcasts",
        Route::Podcast(_) | Route::Search => "unknown",
    };
    format!("nav-{name}")
}

/// The element id of the `index`th podcast in the sidebar.
pub fn podcast_element_id(index: usize) -> String {
    format!("sidebar-podcast-{index}")
}
