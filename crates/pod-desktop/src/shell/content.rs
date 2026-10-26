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
use crate::search::view::SearchView;
use crate::shell::sync_status::{self, SyncCounts};
use crate::sync::session::Account;

pub fn render(
    route: Route,
    inbox: &Entity<InboxView>,
    search: &Entity<SearchView>,
    account: &Account,
    sync_status: &SharedString,
    sync_counts: SyncCounts,
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
        Route::Settings => return settings(account, sync_status, sync_counts, cx),
        Route::Search => return search.clone().into_any_element(),
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

/// The Settings page, so far the account and the sync status.
// TODO: playback and download settings (wireframe D5).
fn settings(
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
