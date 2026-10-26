//! The content pane's page that isn't a view of its own: Settings. The app
//! picks the page for the current route.

use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};

use crate::account;
use crate::app::PodApp;
use crate::shell::sync_status::{self, SyncCounts};
use crate::sync::session::Account;

/// The Settings page, so far the account and the sync status.
// TODO: playback and download settings (wireframe D5).
pub fn settings(
    account: &Account,
    sync_status: &SharedString,
    sync_counts: SyncCounts,
    cx: &mut Context<PodApp>,
) -> AnyElement {
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
        .child(settings_sync(sync_status, sync_counts, cx))
        .into_any_element()
}

/// Settings › Sync, where the title bar's status leads: the status again,
/// the outbox and rejected-log counts (spec 5.6), and a sync button.
// TODO(KIL-52): list each change that couldn't be saved with its reason, and
// the backfill progress and last error from wireframe D5.
fn settings_sync(
    sync_status: &SharedString,
    sync_counts: SyncCounts,
    cx: &mut Context<PodApp>,
) -> impl IntoElement {
    let counts = sync_status::counts_label(sync_counts);
    v_flex()
        .id("settings-sync")
        .gap_2()
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("Sync"),
        )
        .child(
            h_flex()
                .gap_4()
                .child(
                    v_flex()
                        .flex_1()
                        .child(
                            div()
                                .id("settings-sync-status")
                                .aria_label(sync_status.clone())
                                .test_support()
                                .text_sm()
                                .child(sync_status.clone()),
                        )
                        .children(counts.map(|counts| {
                            let counts = SharedString::from(counts);
                            div()
                                .id("settings-sync-counts")
                                .aria_label(counts.clone())
                                .test_support()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(counts)
                        })),
                )
                .child(
                    Button::new("settings-sync-now")
                        .outline()
                        .label("Sync now")
                        .on_click(|_, _, cx| sync_status::sync_now(cx)),
                ),
        )
}
