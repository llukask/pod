//! Title bar with the app name, the sync status and the refresh button (D1
//! annotation 1, D7 annotation 1).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, TitleBar, h_flex};
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div,
};

use super::sync_status;
use crate::account;
use crate::app::{PodApp, Route};
use crate::sync::engine::EngineState;
use crate::sync::session::Account;

/// `status` is the label [`sync_status::status_label`] gives `state`.
pub fn render(
    account: Option<&Account>,
    state: &EngineState,
    status: &SharedString,
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
            .children(account.map(|account| render_sync(account, state, status, cx))),
    )
}

fn render_sync(
    account: &Account,
    state: &EngineState,
    status: &SharedString,
    cx: &mut Context<PodApp>,
) -> AnyElement {
    let icon = match state {
        EngineState::Idle { .. } => IconName::Check,
        EngineState::Running { .. } => IconName::ArrowUpDown,
        EngineState::Offline => IconName::CloudOff,
        EngineState::BackingOff { .. } | EngineState::SignedOut => IconName::TriangleAlert,
    };
    let status = Button::new("sync-status")
        .ghost()
        .xsmall()
        .icon(icon)
        .label(status.clone());
    // Signed out, the status is the way back in (spec 5.7); otherwise it
    // leads to the details in Settings › Sync.
    let status = if *state == EngineState::SignedOut {
        let account = account.clone();
        status.on_click(move |_, window, cx| {
            account::open_sign_in_sheet(account.clone(), window, cx);
        })
    } else {
        status.on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Settings, cx)))
    };
    h_flex()
        .gap_1()
        .child(status)
        .child(
            Button::new("sync-now")
                .ghost()
                .xsmall()
                .icon(IconName::RefreshCw)
                .tooltip("Sync now (Ctrl+R)")
                .on_click(|_, _, cx| sync_status::sync_now(cx)),
        )
        .into_any_element()
}
