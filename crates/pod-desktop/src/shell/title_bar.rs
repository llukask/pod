//! Title bar with the app name and the sync status slot (D1 annotation 1).

use gpui_kit::component::{ActiveTheme as _, TitleBar, h_flex};
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    TestSupportExt as _, div,
};

use crate::app::PodApp;

pub fn render(cx: &mut Context<PodApp>) -> impl IntoElement {
    TitleBar::new().child(
        h_flex()
            .gap_3()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Pod"),
            )
            // TODO: show the sync engine's state (spec 5.6, wireframe D7) and
            // open Settings › Sync on click.
            .child(
                div()
                    .id("sync-status")
                    .test_support()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Not synced"),
            ),
    )
}
