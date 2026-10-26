//! The content pane: the list and detail panes of the current route. The
//! inbox is its own view; the other routes are placeholders that later
//! issues replace with the panes from wireframes D2 to D5.

use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};

use crate::account;
use crate::app::{PodApp, Route};
use crate::inbox::view::InboxView;
use crate::sync::session::Account;

pub fn render(
    route: Route,
    inbox: &Entity<InboxView>,
    account: &Account,
    cx: &mut Context<PodApp>,
) -> AnyElement {
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
        Route::Settings => return settings(account, cx),
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

/// The Settings page, so far only the account and signing out.
// TODO: sync, playback and download settings (wireframe D5).
fn settings(account: &Account, _: &mut Context<PodApp>) -> AnyElement {
    v_flex()
        .id("content")
        .flex_1()
        .min_w_0()
        .h_full()
        .p_6()
        .gap_4()
        .child(
            div()
                .id("page-title")
                .aria_label("Settings")
                .test_support()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("Settings"),
        )
        .child(
            h_flex()
                .gap_4()
                .child(div().flex_1().text_sm().child(format!(
                    "Signed in as {} on {}",
                    account.username, account.server_url
                )))
                .child(
                    Button::new("sign-out")
                        .outline()
                        .label("Sign out")
                        .on_click(|_, window, cx| account::confirm_logout(window, cx)),
                ),
        )
        .into_any_element()
}
