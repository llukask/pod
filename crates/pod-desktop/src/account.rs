//! Signing in and out in the window: the sign-in card (wireframe D6), the
//! password sheet behind "Signed out" and the sign-out confirmation (D7).
//! The work itself is [`Session`]'s, run on the tokio runtime.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, Window, div, px, relative,
};

use crate::runtime::Background;
use crate::store::Store;
use crate::sync::session::{
    Account, Credentials, LogoutSummary, Session, format_bytes, load_logout_summary,
};

// ==============================================================================
// The sign-in form
// ==============================================================================

/// Which account the form signs in to.
#[derive(Clone, Debug)]
enum Mode {
    /// No account: the full card, any server and user, or a new account.
    New,
    /// The password prompt after a `401`: the same server and user, so the
    /// outbox carries over (spec 5.7).
    Again(Account),
}

pub struct SignInForm {
    mode: Mode,
    server_url: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    register: bool,
    busy: bool,
    error: Option<SharedString>,
    _enter: Subscription,
}

impl std::fmt::Debug for SignInForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInForm")
            .field("mode", &self.mode)
            .field("register", &self.register)
            .field("busy", &self.busy)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl SignInForm {
    /// The card shown while there is no account. Requires the [`Session`]
    /// and [`Background`] globals.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_mode(Mode::New, window, cx)
    }

    fn with_mode(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_url = cx.new(|cx| InputState::new(window, cx).placeholder("pod.example.com"));
        let username = cx.new(|cx| InputState::new(window, cx));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let enter = cx.subscribe_in(&password, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(window, cx);
            }
        });
        Self {
            mode,
            server_url,
            username,
            password,
            register: false,
            busy: false,
            error: None,
            _enter: enter,
        }
    }

    fn credentials(&self, cx: &App) -> Credentials {
        let (server_url, username) = match &self.mode {
            Mode::New => (
                self.server_url.read(cx).value().to_string(),
                self.username.read(cx).value().to_string(),
            ),
            Mode::Again(account) => (account.server_url.clone(), account.username.clone()),
        };
        Credentials {
            server_url,
            username,
            password: self.password.read(cx).value().to_string(),
            register: self.register,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let session = cx.global::<Session>().clone();
        let credentials = self.credentials(cx);
        let task = cx
            .global::<Background>()
            .spawn(async move { session.sign_in(credentials).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    // Without an account the window swaps the card for the
                    // app once the row lands; the sheet closes itself.
                    Ok(Ok(_)) => {
                        if let Mode::Again(_) = this.mode {
                            window.close_sheet(cx);
                        }
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) => this.error = Some(format!("Signing in stopped: {error}").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for SignInForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = |text: &'static str| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };
        let submit_label = match (&self.mode, self.register) {
            (_, _) if self.busy => "Signing in…",
            (Mode::New, true) => "Create account",
            _ => "Sign in",
        };
        let form = v_flex().id("sign-in").test_support().gap_2();
        let form = match &self.mode {
            Mode::New => {
                let heading = if self.register {
                    "Create a Pod account"
                } else {
                    "Sign in to Pod"
                };
                form.child(
                    div()
                        .id("sign-in-heading")
                        .aria_label(heading)
                        .test_support()
                        .mb_1()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_center()
                        .child(heading),
                )
                .child(label("Server"))
                .child(Input::new(&self.server_url).id("sign-in-server"))
                .child(label("Username"))
                .child(Input::new(&self.username).id("sign-in-username"))
            }
            Mode::Again(account) => form.child(div().text_sm().child(format!(
                "The server ended this session. Sign in as {} on {} to sync again. \
                         Changes made since then are kept and sent once you're back.",
                account.username, account.server_url
            ))),
        };
        form.child(label("Password"))
            .child(Input::new(&self.password).id("sign-in-password"))
            .when_some(self.error.clone(), |form, error| {
                form.child(
                    div()
                        .id("sign-in-error")
                        .aria_label(error.clone())
                        .test_support()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(div().h(px(8.)))
            .child(
                Button::new("sign-in-submit")
                    .primary()
                    .w_full()
                    .label(submit_label)
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            )
            .when(matches!(self.mode, Mode::New), |form| {
                let toggle = if self.register {
                    "I have an account"
                } else {
                    "Create account"
                };
                form.child(
                    Button::new("sign-in-toggle")
                        .ghost()
                        .w_full()
                        .label(toggle)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.register = !this.register;
                            this.error = None;
                            cx.notify();
                        })),
                )
            })
    }
}

/// The sign-in card, near the middle of the window (D6).
pub fn render_card(form: &Entity<SignInForm>, cx: &App) -> impl IntoElement {
    // The card's top sits at a fixed share of the height instead of the
    // card being centred, so an error line grows it downwards and the
    // fields stay under the cursor.
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .child(div().flex_none().h(relative(0.2)))
        .child(
            v_flex()
                .w(px(340.))
                .p_6()
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(form.clone()),
        )
}

/// Opens the password prompt as a sheet (D7 annotation 1).
pub fn open_sign_in_sheet(account: Account, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| SignInForm::with_mode(Mode::Again(account), window, cx));
    window.open_sheet(cx, move |sheet, _, _| {
        sheet.title("Signed out").child(form.clone())
    });
}

// ==============================================================================
// Signing out
// ==============================================================================

/// What the confirmation says logout throws away (D7 annotation 3).
pub fn logout_message(summary: LogoutSummary) -> String {
    let downloads = (summary.download_bytes > 0).then(|| {
        format!(
            "{} of downloads on this computer",
            format_bytes(summary.download_bytes)
        )
    });
    match (summary.unsent, downloads) {
        (0, None) => "You can sign in again at any time.".to_owned(),
        (0, Some(downloads)) => format!("Signing out deletes {downloads}."),
        (unsent, downloads) => {
            let (changes, them) = if unsent == 1 {
                ("1 change hasn't".to_owned(), "it")
            } else {
                (format!("{unsent} changes haven't"), "them")
            };
            let along = downloads.map_or(String::new(), |downloads| {
                format!(", along with {downloads}")
            });
            format!("{changes} reached the server yet. Signing out now discards {them}{along}.")
        }
    }
}

/// Counts what logout would throw away, then asks.
pub fn confirm_logout(window: &mut Window, cx: &mut App) {
    let store = cx.global::<Store>().clone();
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let summary = match store.read(load_logout_summary).await {
            Ok(summary) => summary,
            Err(error) => {
                tracing::error!("counting what logout discards failed: {error:#}");
                LogoutSummary::default()
            }
        };
        let _ = cx.update_window(handle, |_, window, cx| {
            open_logout_dialog(summary, window, cx);
        });
    })
    .detach();
}

fn open_logout_dialog(summary: LogoutSummary, window: &mut Window, cx: &mut App) {
    let message = SharedString::from(logout_message(summary));
    let confirm = if summary.unsent > 0 {
        "Sign out anyway"
    } else {
        "Sign out"
    };
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Sign out?")
            .child(
                div()
                    .id("sign-out-message")
                    .aria_label(message.clone())
                    .test_support()
                    .text_sm()
                    .child(message.clone()),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                    .child(
                        DialogAction::new()
                            .child(Button::new("sign-out-confirm").primary().label(confirm)),
                    ),
            )
            .on_ok(|_, _, cx| {
                let session = cx.global::<Session>().clone();
                // The window follows the account row, so nothing waits for
                // this.
                cx.global::<Background>().spawn(async move {
                    if let Err(error) = session.logout().await {
                        tracing::error!("logout failed: {error:#}");
                    }
                });
                true
            })
    });
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::http::StatusCode;
    use axum::response::{IntoResponse as _, Response};
    use color_eyre::eyre::WrapErr as _;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use pod_protocol::{DeviceId, ErrorBody, ErrorCode, Timestamp, TokenResponse};
    use rusqlite::Transaction;
    use tokio::sync::oneshot;

    use super::*;
    use crate::app::Route;
    use crate::app::tests::{TestApp, signed_in};
    use crate::shell::sidebar::page_element_id;
    use crate::sync::engine::EngineState;
    use crate::sync::local_write::{self, LocalChange};
    use crate::test_port::bind_test_port;

    const PASSWORD: &str = "correct horse battery";
    /// The user [`signed_in`] seeds.
    const ALICE: &str = "019a3f10-8c2e-7a41-9b3d-ffffffffffff";

    // ==========================================================================
    // A stub server
    // ==========================================================================

    /// Answers login and register as alice when the password is right, and
    /// every sync with `401`, as after a revoked session. Records the paths
    /// it was asked for.
    struct Stub {
        base_url: String,
        paths: Arc<Mutex<Vec<String>>>,
        _stop: oneshot::Sender<()>,
    }

    impl Stub {
        /// Serves on a port from [`bind_test_port`] (`POD_TEST_ACCOUNT_PORT`
        /// overrides the choice) and points the seeded account, if any, at
        /// the stub.
        fn start(app: &TestApp) -> Stub {
            let listener = bind_test_port("POD_TEST_ACCOUNT_PORT");
            let base_url = format!(
                "http://{}",
                listener
                    .local_addr()
                    .expect("a bound listener has an address")
            );
            let server_url = base_url.clone();
            app.runtime
                .block_on(app.store.write(move |transaction| {
                    transaction
                        .execute("update account set server_url = ?1", [server_url])
                        .context("point the account at the stub")
                }))
                .expect("the account update is written");
            let paths = Arc::new(Mutex::new(Vec::new()));
            let recorded = paths.clone();
            let app_router =
                axum::Router::new().fallback(move |request: axum::extract::Request| {
                    let recorded = recorded.clone();
                    async move {
                        let path = request.uri().path().to_owned();
                        recorded.lock().expect("not poisoned").push(path.clone());
                        let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                            .await
                            .expect("the body arrives");
                        answer(&path, &body)
                    }
                });
            let listener = {
                let _runtime = app.runtime.enter();
                tokio::net::TcpListener::from_std(listener).expect("the listener joins the runtime")
            };
            let (stop, stopped) = oneshot::channel::<()>();
            app.background.spawn(async move {
                axum::serve(
                    listener,
                    app_router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("the stub runs");
            });
            Stub {
                base_url,
                paths,
                _stop: stop,
            }
        }

        fn paths(&self) -> Vec<String> {
            self.paths.lock().expect("not poisoned").clone()
        }
    }

    fn answer(path: &str, body: &[u8]) -> Response {
        let refuse =
            |status, code| (status, axum::Json(ErrorBody::new(code, "refused"))).into_response();
        match path {
            "/v1/auth/login" | "/v1/auth/register" => {
                let request: pod_protocol::AuthRequest =
                    serde_json::from_slice(body).expect("the app sends an auth request");
                if request.password != PASSWORD {
                    return refuse(StatusCode::UNAUTHORIZED, ErrorCode::InvalidCredentials);
                }
                axum::Json(TokenResponse {
                    token: "fresh-token".to_owned(),
                    device_id: DeviceId::new_v7(),
                    user_id: ALICE.parse().expect("literal is a UUID"),
                    expires_at: Timestamp::now(),
                })
                .into_response()
            }
            "/v1/auth/logout" => StatusCode::NO_CONTENT.into_response(),
            _ => refuse(StatusCode::UNAUTHORIZED, ErrorCode::Unauthenticated),
        }
    }

    fn count(app: &TestApp, table: &'static str) -> i64 {
        app.read(move |connection| {
            connection
                .query_row(&format!("select count(*) from {table}"), [], |row| {
                    row.get(0)
                })
                .context("count the rows")
        })
    }

    fn token(app: &TestApp) -> String {
        app.read(|connection| {
            connection
                .query_row("select token from account", [], |row| row.get(0))
                .context("read the token")
        })
    }

    /// One queued change, as any local edit leaves behind.
    fn queue_a_change(app: &TestApp) {
        let feed_url = "https://feeds.example.com/show.xml";
        let normalized = pod_protocol::normalize_feed_url(feed_url).expect("the URL is valid");
        let change = LocalChange::Subscription {
            podcast_id: pod_protocol::PodcastId::from_feed_url(&normalized),
            feed_url: feed_url.to_owned(),
            subscribed: true,
        };
        app.runtime
            .block_on(local_write::write(&app.store, &app.sync, change))
            .expect("the local write commits");
    }

    // ==========================================================================
    // Signing in
    // ==========================================================================

    #[gpui_kit::test]
    fn the_card_registers_stores_the_account_and_asks_for_the_first_sync(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, |_| Ok(()));
        let stub = Stub::start(&app);
        app.wait_until(cx, "the sign-in card shows", |window| {
            window.try_find("sign-in-server").is_some()
        });
        assert!(app.sync.wanted().borrow().is_none());

        cx.update_window(app.window, |_, window, cx| {
            assert!(
                window.try_find("player-bar").is_none(),
                "no app without an account"
            );
            window.click("sign-in-server", cx);
            window.input(&stub.base_url, cx);
            window.click("sign-in-username", cx);
            window.input("Alice", cx);
            window.click("sign-in-password", cx);
            window.input(PASSWORD, cx);
            window.click("sign-in-toggle", cx);
            window.click("sign-in-submit", cx);
        })
        .expect("the window is still open");

        app.wait_until(cx, "the app replaces the card", |window| {
            window.try_find("player-bar").is_some()
        });
        assert_eq!(stub.paths(), ["/v1/auth/register"]);
        assert_eq!(token(&app), "fresh-token");
        let username: String = app.read(|connection| {
            connection
                .query_row("select username from account", [], |row| row.get(0))
                .context("read the username")
        });
        assert_eq!(username, "alice");
        assert!(
            app.sync.wanted().borrow().is_some(),
            "the first sync is asked for"
        );
        app.wait_for_label(cx, "inbox-loading", "Looking up your subscriptions");
    }

    #[gpui_kit::test]
    fn the_heading_follows_the_mode(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, |_| Ok(()));
        app.wait_for_label(cx, "sign-in-heading", "Sign in to Pod");
        cx.update_window(app.window, |_, window, cx| {
            window.click("sign-in-toggle", cx);
            assert_eq!(
                window.find("sign-in-heading").label(),
                Some("Create a Pod account")
            );
            window.click("sign-in-toggle", cx);
            assert_eq!(
                window.find("sign-in-heading").label(),
                Some("Sign in to Pod")
            );
        })
        .expect("the window is still open");
    }

    #[gpui_kit::test]
    fn a_wrong_password_shows_why_and_stores_nothing(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, |_| Ok(()));
        let stub = Stub::start(&app);
        app.wait_until(cx, "the sign-in card shows", |window| {
            window.try_find("sign-in-server").is_some()
        });
        let password_before = cx
            .update_window(app.window, |_, window, cx| {
                window.click("sign-in-server", cx);
                window.input(&stub.base_url, cx);
                window.click("sign-in-username", cx);
                window.input("alice", cx);
                window.click("sign-in-password", cx);
                window.input("wrong", cx);
                let password = window.find("sign-in-password").bounds();
                window.press("enter", cx);
                password
            })
            .expect("the window is still open");
        app.wait_until(cx, "the error shows", |window| {
            window.try_find("sign-in-error").is_some()
        });
        cx.update_window(app.window, |_, window, _| {
            assert_eq!(
                window.find("sign-in-error").label(),
                Some("Username or password is wrong.")
            );
            assert_eq!(
                window.find("sign-in-password").bounds(),
                password_before,
                "the error line doesn't move the field under the cursor"
            );
        })
        .expect("the window is still open");
        assert_eq!(count(&app, "account"), 0);
    }

    // ==========================================================================
    // Signed out
    // ==========================================================================

    #[gpui_kit::test]
    fn a_revoked_session_shows_signed_out_and_the_sheet_resumes_it(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in);
        let _stub = Stub::start(&app);
        queue_a_change(&app);
        let state = app
            .runtime
            .block_on(app.engine.sync())
            .expect("the local store works");
        assert_eq!(state, EngineState::SignedOut);

        app.wait_for_label(cx, "sync-status", "Signed out · Sign in");
        cx.update_window(app.window, |_, window, cx| {
            assert!(window.find("player-bar").visible(), "the app stays usable");
            window.click("sync-status", cx);
        })
        .expect("the window is still open");
        app.wait_until(cx, "the password sheet opens", |window| {
            window.try_find("sign-in-password").is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            assert!(
                window.try_find("sign-in-server").is_none(),
                "same server and user"
            );
            window.click("sign-in-password", cx);
            window.input(PASSWORD, cx);
            window.press("enter", cx);
        })
        .expect("the window is still open");

        app.wait_until(cx, "the sheet closes", |window| {
            window.try_find("sign-in-password").is_none()
        });
        assert_eq!(token(&app), "fresh-token");
        assert_eq!(count(&app, "outbox"), 1, "the outbox carries over");
    }

    // ==========================================================================
    // Signing out
    // ==========================================================================

    fn with_downloads(transaction: &Transaction<'_>) -> color_eyre::Result<()> {
        signed_in(transaction)?;
        transaction
            .execute_batch(
                "insert into device_setting (key, value) values ('volume', '0.8');
                 insert into download (episode_id, state, audio_url, path, bytes_done)
                 values ('e', 'complete', 'https://media.example.com/e.mp3', 'p/e.mp3',
                         1400000000);",
            )
            .context("insert a download and a device setting")
    }

    #[gpui_kit::test]
    fn signing_out_says_what_goes_and_returns_to_the_card(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, with_downloads);
        let stub = Stub::start(&app);
        queue_a_change(&app);
        app.wait_until(cx, "the app shows", |window| {
            window.try_find("player-bar").is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            window
                .within("sidebar")
                .click(page_element_id(Route::Settings), cx);
            window.click("sign-out", cx);
        })
        .expect("the window is still open");

        app.wait_until(cx, "the confirmation opens", |window| {
            window.try_find("sign-out-message").is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            assert_eq!(
                window.find("sign-out-message").label(),
                Some(
                    "1 change hasn't reached the server yet. Signing out now discards it, \
                     along with 1.4 GB of downloads on this computer."
                )
            );
            window.within("dialog").click("sign-out-confirm", cx);
        })
        .expect("the window is still open");

        app.wait_until(cx, "the card is back", |window| {
            window.try_find("sign-in-server").is_some()
        });
        for table in ["account", "outbox", "download", "subscription"] {
            assert_eq!(count(&app, table), 0, "{table}");
        }
        assert_eq!(count(&app, "device_setting"), 1);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !stub.paths().contains(&"/v1/auth/logout".to_owned()) {
            assert!(
                std::time::Instant::now() < deadline,
                "logout tells the server"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_confirmation_names_what_goes() {
        let summary = |unsent, download_bytes| LogoutSummary {
            unsent,
            download_bytes,
        };
        assert_eq!(
            logout_message(summary(3, 1_400_000_000)),
            "3 changes haven't reached the server yet. Signing out now discards them, \
             along with 1.4 GB of downloads on this computer."
        );
        assert_eq!(
            logout_message(summary(1, 0)),
            "1 change hasn't reached the server yet. Signing out now discards it."
        );
        assert_eq!(
            logout_message(summary(0, 2_000_000)),
            "Signing out deletes 2.0 MB of downloads on this computer."
        );
        assert_eq!(
            logout_message(summary(0, 0)),
            "You can sign in again at any time."
        );
    }
}
