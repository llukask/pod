//! The sidebar (D1 annotation 2): the fixed pages, then the Podcasts group
//! with each subscription's unplayed count.

use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{ActiveTheme as _, Collapsible, IconName, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div,
};

use crate::app::{PodApp, Route};
use crate::inbox::SidebarPodcast;
use crate::search::view::SearchView;

/// The fixed pages, top to bottom.
const PAGES: [(&str, IconName, Route); 3] = [
    ("Inbox", IconName::Inbox, Route::Inbox),
    ("Downloads", IconName::ArrowDown, Route::Downloads),
    ("Settings", IconName::Settings, Route::Settings),
];

pub fn render(
    current: Route,
    podcasts: &[SidebarPodcast],
    search: &Entity<SearchView>,
    cx: &mut Context<PodApp>,
) -> impl IntoElement {
    // Every inbox episode belongs to a subscribed podcast, so the counts
    // add up to the inbox's.
    let inbox_count: usize = podcasts.iter().map(|podcast| podcast.unplayed).sum();
    let pages = SidebarMenu::new().children(PAGES.map(|(label, icon, target)| {
        let item = SidebarMenuItem::new(label)
            .icon(icon)
            .active(current == target)
            .on_click(cx.listener(move |app, _, _, cx| app.navigate(target, cx)));
        if target == Route::Inbox {
            item.suffix(move |_, cx| count_badge("inbox-count", inbox_count, cx))
        } else {
            item
        }
    }));
    // TODO: the pending and failing markers (D1 annotation 2), and artwork.
    let subscriptions = SidebarMenu::new().children(podcasts.iter().map(|podcast| {
        let target = Route::Podcast(podcast.id);
        let badge_id = SharedString::from(format!("podcast-unplayed-{}", podcast.id));
        let unplayed = podcast.unplayed;
        SidebarMenuItem::new(podcast.title.clone())
            .active(current == target)
            .on_click(cx.listener(move |app, _, _, cx| app.navigate(target, cx)))
            .suffix(move |_, cx| count_badge(badge_id.clone(), unplayed, cx))
    }));

    Sidebar::new("sidebar")
        .collapsible(SidebarCollapsible::None)
        .header(search.read(cx).render_field(cx))
        .child(Section::Pages(Box::new(pages)))
        .child(Section::Podcasts {
            empty: podcasts.is_empty(),
            menu: Box::new(subscriptions),
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
        .id(ElementId::Name(id.into()))
        .aria_label(label.clone())
        .test_support()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

/// The element id gpui-component gives the sidebar item for `route`:
/// `<section index>-<item index>`, with the pages as section 0. The item's
/// accessible label sits on an unobserved child, so tests can't find it by
/// label.
#[cfg(test)]
pub fn page_element_id(route: Route) -> String {
    let index = PAGES
        .iter()
        .position(|(_, _, target)| *target == route)
        .expect("only fixed pages have a sidebar item");
    format!("0-{index}")
}

/// `Sidebar` holds one item type, so the two kinds of section share this
/// enum.
#[derive(Clone)]
enum Section {
    Pages(Box<SidebarMenu>),
    Podcasts { menu: Box<SidebarMenu>, empty: bool },
}

impl Collapsible for Section {
    fn is_collapsed(&self) -> bool {
        match self {
            Self::Pages(menu) | Self::Podcasts { menu, .. } => menu.is_collapsed(),
        }
    }

    // The sidebar is built with `SidebarCollapsible::None`, so this only
    // forwards to keep the pages menu consistent if that changes.
    fn collapsed(self, collapsed: bool) -> Self {
        match self {
            Self::Pages(menu) => Self::Pages(Box::new(menu.collapsed(collapsed))),
            Self::Podcasts { menu, empty } => Self::Podcasts {
                menu: Box::new(menu.collapsed(collapsed)),
                empty,
            },
        }
    }
}

impl SidebarItem for Section {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        match self {
            Self::Pages(menu) => menu.render(id, window, cx).into_any_element(),
            Self::Podcasts { menu, empty } => v_flex()
                .child(
                    SidebarGroup::new("Podcasts")
                        .child(*menu)
                        .render(id, window, cx),
                )
                .when(empty, |this| {
                    this.child(
                        div()
                            .id("podcasts-empty")
                            .test_support()
                            .px_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No podcasts yet"),
                    )
                })
                .into_any_element(),
        }
    }
}
