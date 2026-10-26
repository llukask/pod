//! The sidebar (D1 annotation 2): the fixed pages, then the Podcasts group
//! with each subscription's unplayed count.

use std::rc::Rc;

use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{ActiveTheme as _, Collapsible, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::app::{PodApp, Route};
use crate::inbox::SidebarPodcast;
use crate::search::view::SearchView;
use crate::shell::artwork::artwork;

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
    // TODO: the pending and failing markers (D1 annotation 2).
    let subscriptions = PodcastMenu(
        podcasts
            .iter()
            .map(|podcast| {
                let target = Route::Podcast(podcast.id);
                PodcastItem {
                    podcast: podcast.clone(),
                    active: current == target,
                    on_click: Rc::new(cx.listener(move |app, _, _, cx| app.navigate(target, cx))),
                }
            })
            .collect(),
    );

    Sidebar::new("sidebar")
        .collapsible(SidebarCollapsible::None)
        .header(search.read(cx).render_field(cx))
        .child(Section::Pages(Box::new(pages)))
        .child(Section::Podcasts {
            empty: podcasts.is_empty(),
            menu: subscriptions,
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

/// The element id of the `index`th podcast in the sidebar, in the scheme
/// of [`page_element_id`]: the Podcasts group is section 1, and its menu
/// the group's first child.
#[cfg(test)]
pub fn podcast_element_id(index: usize) -> String {
    format!("1-0-{index}")
}

/// `Sidebar` holds one item type, so the two kinds of section share this
/// enum.
#[derive(Clone)]
enum Section {
    Pages(Box<SidebarMenu>),
    Podcasts { menu: PodcastMenu, empty: bool },
}

// The sidebar is built with `SidebarCollapsible::None`, so the podcasts
// never collapse; the pages menu only forwards to stay consistent if that
// changes.
impl Collapsible for Section {
    fn is_collapsed(&self) -> bool {
        match self {
            Self::Pages(menu) => menu.is_collapsed(),
            Self::Podcasts { .. } => false,
        }
    }

    fn collapsed(self, collapsed: bool) -> Self {
        match self {
            Self::Pages(menu) => Self::Pages(Box::new(menu.collapsed(collapsed))),
            podcasts @ Self::Podcasts { .. } => podcasts,
        }
    }
}

/// The subscriptions. `SidebarMenuItem` only takes an SVG icon, so the rows
/// are drawn here to show artwork, styled like the menu items above them.
#[derive(Clone)]
struct PodcastMenu(Vec<PodcastItem>);

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(Clone)]
struct PodcastItem {
    podcast: SidebarPodcast,
    active: bool,
    on_click: ClickHandler,
}

impl Collapsible for PodcastMenu {
    fn is_collapsed(&self) -> bool {
        false
    }

    fn collapsed(self, _: bool) -> Self {
        self
    }
}

impl SidebarItem for PodcastMenu {
    fn render(self, id: impl Into<ElementId>, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = id.into();
        v_flex()
            .gap_2()
            .children(self.0.into_iter().enumerate().map(|(index, item)| {
                let PodcastItem {
                    podcast,
                    active,
                    on_click,
                } = item;
                let badge_id = SharedString::from(format!("podcast-unplayed-{}", podcast.id));
                div()
                    .id(SharedString::from(format!("{id}-{index}")))
                    .test_support()
                    .w_full()
                    .child(
                        h_flex()
                            .id("item")
                            .role(Role::TreeItem)
                            .aria_label(SharedString::from(podcast.title.clone()))
                            .aria_selected(active)
                            .h_7()
                            .p_2()
                            .gap_x_2()
                            .overflow_x_hidden()
                            .rounded(cx.theme().radius)
                            .text_sm()
                            .when(!active, |this| {
                                this.hover(|this| {
                                    this.bg(cx.theme().sidebar_accent.opacity(0.8))
                                        .text_color(cx.theme().sidebar_accent_foreground)
                                })
                            })
                            .when(active, |this| {
                                this.font_weight(FontWeight::MEDIUM)
                                    .bg(cx.theme().tokens.sidebar_accent)
                                    .text_color(cx.theme().sidebar_accent_foreground)
                            })
                            .child(artwork(
                                SharedString::from(format!("podcast-artwork-{}", podcast.id)),
                                podcast.artwork.as_deref(),
                                px(20.),
                                cx,
                            ))
                            .child(div().flex_1().min_w_0().truncate().child(podcast.title))
                            .child(count_badge(badge_id, podcast.unplayed, cx))
                            .on_click(move |event, window, cx| on_click(event, window, cx)),
                    )
            }))
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
                        .child(menu)
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
