//! Title bar with the app name and the sync status slot (D1 annotation 1).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, TitleBar, h_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    TestSupportExt as _, div,
};

use crate::account;
use crate::app::PodApp;
use crate::sync::engine::EngineState;
use crate::sync::session::Account;

pub fn render(
    account: Option<&Account>,
    state: &EngineState,
    cx: &mut Context<PodApp>,
) -> impl IntoElement {
    TitleBar::new().child(
        h_flex()
            .gap_3()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Pod"),
            )
            .children(account.map(|account| sync_status(account, state, cx))),
    )
}

// TODO(KIL-41): the other states from spec 5.6 ("Synced N min ago",
// "Syncing…", "Offline · N waiting", "Can't reach server · retrying in T"),
// with a click opening Settings › Sync.
fn sync_status(account: &Account, state: &EngineState, cx: &mut Context<PodApp>) -> AnyElement {
    if *state == EngineState::SignedOut {
        let account = account.clone();
        return Button::new("sync-status")
            .ghost()
            .xsmall()
            .icon(IconName::TriangleAlert)
            .label("Signed out · Sign in")
            .on_click(move |_, window, cx| {
                account::open_sign_in_sheet(account.clone(), window, cx);
            })
            .into_any_element();
    }
    div()
        .id("sync-status")
        .test_support()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child("Not synced")
        .into_any_element()
}
