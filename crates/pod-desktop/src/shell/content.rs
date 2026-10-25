//! The content pane: one placeholder page per route. Later issues replace
//! these with the list and detail panes from wireframes D1 to D5.

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};

use crate::app::{PodApp, Route};

pub fn render(route: Route, cx: &mut Context<PodApp>) -> impl IntoElement {
    let (title, body): (SharedString, SharedString) = match route {
        Route::Inbox => (
            "Inbox".into(),
            "New episodes from your podcasts show up here.".into(),
        ),
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
}
