//! The content pane: the list and detail panes of the current route. The
//! inbox is its own view; the other routes are placeholders that later
//! issues replace with the panes from wireframes D2 to D5.

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};

use crate::app::{PodApp, Route};
use crate::inbox::view::InboxView;

pub fn render(route: Route, inbox: &Entity<InboxView>, cx: &mut Context<PodApp>) -> AnyElement {
    let (title, body): (SharedString, SharedString) = match route {
        Route::Inbox => {
            return div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(inbox.clone())
                .into_any_element();
        }
        Route::Downloads => (
            "Downloads".into(),
            "Downloaded episodes and transfers in progress show up here.".into(),
        ),
        Route::Settings => (
            "Settings".into(),
            "Account, sync, playback and download settings go here.".into(),
        ),
        Route::Podcast(id) => ("Podcast".into(), format!("Podcast {id}").into()),
    };

    v_flex()
        .id("content")
        .flex_1()
        .min_w_0()
        .h_full()
        .p_6()
        .gap_2()
        .child(
            div()
                .id("page-title")
                .aria_label(title.clone())
                .test_support()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(body),
        )
        .into_any_element()
}
