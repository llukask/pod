//! The title bar (D1 annotation 1, D7 annotation 1): the logo and "Pod" on
//! the sidebar's colour, so the sidebar runs the window's full height, and
//! the sync status with the refresh button on the right.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _,
    div, px,
};

use super::icon::PodIcon;
use super::layout::SIDEBAR;
use super::sync_status;
use super::ui::{ROUND, icon, logo};
use crate::account;
use crate::app::{PodApp, Route};
use crate::sync::engine::EngineState;
use crate::sync::session::Account;
use crate::theme::{FONT_DISPLAY, palette};

/// The bar's height. Taller than gpui-kit's default, to give the brand room.
const HEIGHT: gpui_kit::Pixels = px(48.);

/// `status` is the label [`sync_status::status_label`] gives `state`.
/// `with_sidebar` paints the brand segment in the sidebar's colour; the
/// sign-in card has no sidebar.
pub fn render(
    account: Option<&Account>,
    state: &EngineState,
    status: &SharedString,
    with_sidebar: bool,
    cx: &mut Context<PodApp>,
) -> impl IntoElement {
    let p = palette(cx);
    TitleBar::new()
        .h(HEIGHT)
        .bg(p.background)
        .border_color(p.background)
        // macOS keeps its padding for the traffic lights.
        .when(!cfg!(target_os = "macos"), |this| this.pl_0())
        .child(
            h_flex()
                .h_full()
                .w(SIDEBAR)
                .flex_none()
                .gap(px(10.))
                .px(px(20.))
                .when(with_sidebar, |this| this.bg(p.sidebar))
                .when(with_sidebar, |this| {
                    this.child(logo(px(26.), cx)).child(
                        div()
                            .font_family(FONT_DISPLAY)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(20.))
                            .child("Pod"),
                    )
                }),
        )
        .child(div().flex_1())
        .children(account.map(|account| render_sync(account, state, status, cx)))
}

fn render_sync(
    account: &Account,
    state: &EngineState,
    status: &SharedString,
    cx: &mut Context<PodApp>,
) -> AnyElement {
    let p = palette(cx);
    let glyph = match state {
        EngineState::Idle { .. } => IconName::Check,
        EngineState::Running { .. } => IconName::ArrowUpDown,
        EngineState::Offline => IconName::CloudOff,
        EngineState::BackingOff { .. } | EngineState::SignedOut => IconName::TriangleAlert,
    };
    let status = Button::new("sync-status")
        .ghost()
        .small()
        .text_color(p.muted)
        .rounded(ROUND)
        .px(px(10.))
        .icon(glyph)
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
        .pr(px(8.))
        .child(status)
        .child(
            Button::new("sync-now")
                .ghost()
                .text_color(p.muted)
                .rounded(ROUND)
                .size(px(28.))
                .child(icon(PodIcon::Refresh, px(16.)))
                .tooltip("Sync now (Ctrl+R)")
                .on_click(|_, _, cx| sync_status::sync_now(cx)),
        )
        .into_any_element()
}
