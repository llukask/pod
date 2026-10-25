//! The sidebar (D1 annotation 2): the fixed pages, then the Podcasts group.

use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{ActiveTheme as _, Collapsible, IconName, v_flex};
use gpui_kit::{
    App, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    TestSupportExt as _, Window, div,
};

use crate::app::{PodApp, Route};

/// The fixed pages, top to bottom.
const PAGES: [(&str, IconName, Route); 3] = [
    ("Inbox", IconName::Inbox, Route::Inbox),
    ("Downloads", IconName::ArrowDown, Route::Downloads),
    ("Settings", IconName::Settings, Route::Settings),
];

pub fn render(current: Route, cx: &mut Context<PodApp>) -> impl IntoElement {
    let pages = SidebarMenu::new().children(PAGES.map(|(label, icon, target)| {
        SidebarMenuItem::new(label)
            .icon(icon)
            .active(current == target)
            .on_click(cx.listener(move |app, _, _, cx| app.navigate(target, cx)))
    }));

    // TODO: the search field ("Search or paste feed URL", wireframe D3) goes
    // above the pages as the sidebar header.
    Sidebar::new("sidebar")
        .collapsible(SidebarCollapsible::None)
        .child(Section::Pages(Box::new(pages)))
        .child(Section::Podcasts)
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
    // TODO: list subscriptions from the local store as `Route::Podcast`
    // items, with unplayed counts and the pending and failing markers.
    Podcasts,
}

impl Collapsible for Section {
    fn is_collapsed(&self) -> bool {
        match self {
            Self::Pages(menu) => menu.is_collapsed(),
            Self::Podcasts => false,
        }
    }

    // The sidebar is built with `SidebarCollapsible::None`, so this only
    // forwards to keep the pages menu consistent if that changes.
    fn collapsed(self, collapsed: bool) -> Self {
        match self {
            Self::Pages(menu) => Self::Pages(Box::new(menu.collapsed(collapsed))),
            Self::Podcasts => Self::Podcasts,
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
            Self::Podcasts => v_flex()
                .child(
                    SidebarGroup::new("Podcasts")
                        .child(SidebarMenu::new())
                        .render(id, window, cx),
                )
                .child(
                    div()
                        .id("podcasts-empty")
                        .test_support()
                        .px_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No podcasts yet"),
                )
                .into_any_element(),
        }
    }
}
