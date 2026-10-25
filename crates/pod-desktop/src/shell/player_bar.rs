//! The player bar along the bottom of the window (D1 annotation 5). It stays
//! visible on every route.

use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    TestSupportExt as _, px,
};

use crate::app::PodApp;

// TODO: the player entity (spec 8) fills this with the current episode,
// transport controls, position, speed and volume.
pub fn render(cx: &mut Context<PodApp>) -> impl IntoElement {
    h_flex()
        .id("player-bar")
        .test_support()
        .flex_shrink_0()
        .h(px(56.))
        .px_4()
        .border_t_1()
        .border_color(cx.theme().border)
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child("Nothing playing")
}
