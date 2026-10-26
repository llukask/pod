//! The root view: which page the window shows, and the shell around it
//! (wireframe D1).

use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, TitleBar, h_flex, v_flex};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Result, SharedString, Styled as _, Task, Window,
    WindowBounds, WindowOptions, px, size,
};
use pod_protocol::{PodcastId, Timestamp};

use crate::account::{self, SignInForm};
use crate::inbox::view::InboxView;
use crate::inbox::{self, SidebarPodcast};
use crate::shell;
use crate::shell::sync_status::{self, SyncCounts, SyncNow};
use crate::store::{Table, live};
use crate::sync::engine::{EngineState, SyncEngine};
use crate::sync::session::{Account, load_account};

/// The page selected in the sidebar. The list and detail panes show its
/// content; the title bar and player bar stay the same for every route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Inbox,
    Downloads,
    Settings,
    Podcast(PodcastId),
}

/// Whether there is an account, from a live query of the `account` row.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Gate {
    /// The first query result hasn't arrived.
    Loading,
    /// No account: the window shows the sign-in card (D6).
    SignIn,
    SignedIn(Account),
}

/// Owns navigation state for the main window, and the pages that keep
/// state across navigation.
#[derive(Debug)]
pub struct PodApp {
    route: Route,
    inbox: Entity<InboxView>,
    /// The sidebar's Podcasts group, from a live query.
    podcasts: Vec<SidebarPodcast>,
    gate: Gate,
    /// The card's form while there is no account, made fresh after each
    /// logout so no password lingers.
    sign_in: Option<Entity<SignInForm>>,
    sync_state: EngineState,
    sync_counts: SyncCounts,
    /// The status label as last rendered. Its time parts ("2 min ago",
    /// "retrying in 4 min") age without a state change, so a ticker
    /// re-renders when the label would read differently.
    sync_status: SharedString,
    _podcasts_query: Task<()>,
    _account_query: Task<()>,
    _sync_state: Task<()>,
    _sync_counts_query: Task<()>,
    _sync_status_ticker: Task<()>,
}

impl PodApp {
    /// Requires the [`crate::store::Store`], [`crate::sync::SyncHandle`],
    /// [`SyncEngine`], [`crate::sync::session::Session`] and
    /// [`crate::runtime::Background`] globals.
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut states = cx.global::<SyncEngine>().state();
        let sync_state = cx.spawn(async move |this, cx| {
            while states.changed().await.is_ok() {
                let state = states.borrow_and_update().clone();
                let updated = this.update(cx, |this, cx| {
                    this.sync_state = state;
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
        });
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let ticked = this.update(cx, |this, cx| {
                    if this.sync_status_label() != this.sync_status.as_ref() {
                        cx.notify();
                    }
                });
                if ticked.is_err() {
                    return;
                }
            }
        });
        Self {
            route: Route::Inbox,
            inbox: cx.new(|cx| InboxView::new(window, cx)),
            podcasts: Vec::new(),
            gate: Gate::Loading,
            sign_in: None,
            sync_state: cx.global::<SyncEngine>().state().borrow().clone(),
            sync_counts: SyncCounts::default(),
            sync_status: SharedString::default(),
            _podcasts_query: live::observe_tables(
                cx,
                inbox::tables(),
                |connection| inbox::load_podcasts(connection, Timestamp::now()),
                |this: &mut Self, podcasts, _| this.podcasts = podcasts,
            ),
            _account_query: live::observe_tables(
                cx,
                Table::Account.into(),
                load_account,
                |this: &mut Self, account, _| {
                    this.gate = match account {
                        Some(account) => {
                            this.sign_in = None;
                            Gate::SignedIn(account)
                        }
                        None => {
                            this.route = Route::Inbox;
                            Gate::SignIn
                        }
                    };
                },
            ),
            _sync_state: sync_state,
            _sync_counts_query: live::observe_tables(
                cx,
                sync_status::tables(),
                sync_status::load_counts,
                |this: &mut Self, counts, _| this.sync_counts = counts,
            ),
            _sync_status_ticker: ticker,
        }
    }

    fn sync_status_label(&self) -> String {
        sync_status::status_label(
            &self.sync_state,
            self.sync_counts.waiting,
            Instant::now(),
            Timestamp::now(),
        )
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            cx.notify();
        }
    }
}

impl Render for PodApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        self.sync_status = self.sync_status_label().into();
        let account = match &self.gate {
            Gate::Loading => {
                return root.child(shell::title_bar::render(
                    None,
                    &self.sync_state,
                    &self.sync_status,
                    cx,
                ));
            }
            Gate::SignIn => {
                let form = self
                    .sign_in
                    .get_or_insert_with(|| cx.new(|cx| SignInForm::new(window, cx)));
                return root
                    .child(shell::title_bar::render(
                        None,
                        &self.sync_state,
                        &self.sync_status,
                        cx,
                    ))
                    .child(account::render_card(form, cx));
            }
            Gate::SignedIn(account) => account,
        };
        root.on_action(cx.listener(|_, _: &SyncNow, _, cx| sync_status::sync_now(cx)))
            .child(shell::title_bar::render(
                Some(account),
                &self.sync_state,
                &self.sync_status,
                cx,
            ))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(shell::sidebar::render(self.route, &self.podcasts, cx))
                    .child(shell::content::render(
                        self.route,
                        &self.inbox,
                        account,
                        &self.sync_status,
                        self.sync_counts,
                        cx,
                    )),
            )
            .child(shell::player_bar::render(cx))
    }
}

/// Registers the key bindings of the window's pages.
pub fn init(cx: &mut App) {
    inbox::view::init(cx);
    sync_status::init(cx);
}

/// Opens the main window. Requires `gpui_kit::init`, [`init`] and the
/// globals [`PodApp::new`] needs.
pub fn open_main_window(cx: &mut App) -> Result<AnyWindowHandle> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1100.), px(720.)),
            cx,
        ))),
        window_min_size: Some(size(px(640.), px(420.))),
        app_id: Some("pod".into()),
        ..TitleBar::window_options()
    };
    let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| PodApp::new(window, cx))
    })?;
    Ok(window)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::{Duration, Instant};

    use color_eyre::eyre::Result as EyreResult;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use rusqlite::Transaction;
    use tempfile::TempDir;

    use tokio::sync::watch;

    use super::*;
    use crate::download::{self, Downloader};
    use crate::net::NetworkState;
    use crate::reconcile::Reconciler;
    use crate::runtime::{self, Background};
    use crate::store::Store;
    use crate::store::tests::scratch_dir;
    use crate::sync::SyncHandle;
    use crate::sync::session::Session;

    /// The main window over a scratch store, with the sync engine and the
    /// session but no driver: tests run syncs themselves.
    pub(crate) struct TestApp {
        _dir: TempDir,
        pub store: Store,
        pub sync: SyncHandle,
        pub engine: SyncEngine,
        /// The app's background runtime. Also runs store calls from the test
        /// thread, outside the foreground executor.
        pub runtime: tokio::runtime::Runtime,
        pub background: Background,
        pub window: AnyWindowHandle,
    }

    impl TestApp {
        /// Opens the window after `seed` filled the store.
        pub fn open(
            cx: &mut TestAppContext,
            seed: impl FnOnce(&Transaction<'_>) -> EyreResult<()> + Send + 'static,
        ) -> TestApp {
            // The store's threads wake the window's tasks from outside the
            // test scheduler, which it otherwise reports as non-determinism.
            cx.executor().allow_parking();
            let dir = scratch_dir();
            let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
            let (runtime, background) = runtime::start().expect("a runtime builds in tests");
            runtime
                .block_on(store.write(seed))
                .expect("the seed data is written");
            let sync = SyncHandle::default();
            let (_, network) = watch::channel(NetworkState::ASSUMED);
            let engine = SyncEngine::new(store.clone(), sync.clone(), network.clone())
                .expect("the engine builds");
            // Not running: logout only cancels its (absent) transfers and
            // deletes its root.
            let downloader = Downloader {
                store: store.clone(),
                client: download::client_builder()
                    .no_proxy()
                    .build()
                    .expect("the test client builds"),
                root: dir.path().join("audio"),
                network,
                checkpoint_every: Duration::ZERO,
            };
            let session = Session::new(
                store.clone(),
                sync.clone(),
                engine.clone(),
                Reconciler::new(downloader, |_| Ok(1 << 40)),
            );
            cx.update(|cx| {
                gpui_kit::init(cx);
                init(cx);
                cx.set_global(store.clone());
                cx.set_global(sync.clone());
                cx.set_global(engine.clone());
                cx.set_global(session);
                cx.set_global(background.clone());
            });
            let window = cx
                .update(open_main_window)
                .expect("the test platform opens windows");
            TestApp {
                _dir: dir,
                store,
                sync,
                engine,
                runtime,
                background,
                window,
            }
        }

        /// Renders frames until `done` holds. Query results arrive from the
        /// store's read thread in real time, so this also waits.
        pub fn wait_until(
            &self,
            cx: &mut TestAppContext,
            what: &str,
            done: impl Fn(&mut Window) -> bool,
        ) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                cx.run_until_parked();
                let ready = cx
                    .update_window(self.window, |_, window, cx| {
                        window.render_frame(cx);
                        done(window)
                    })
                    .expect("the window is still open");
                if ready {
                    return;
                }
                assert!(Instant::now() < deadline, "timed out waiting until {what}");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        pub fn read<T: Send + 'static>(
            &self,
            read: impl FnOnce(&rusqlite::Connection) -> EyreResult<T> + Send + 'static,
        ) -> T {
            self.runtime
                .block_on(self.store.read(read))
                .expect("the read runs")
        }
    }

    /// A signed-in account and nothing else.
    pub(crate) fn signed_in(transaction: &Transaction<'_>) -> EyreResult<()> {
        use color_eyre::eyre::WrapErr as _;
        transaction
            .execute(
                "insert into account (id, server_url, user_id, username, device_id, token)
                 values (1, 'http://127.0.0.1:3004', '019a3f10-8c2e-7a41-9b3d-ffffffffffff',
                         'alice', '019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d', 'secret')",
                [],
            )
            .context("insert the account")?;
        Ok(())
    }

    #[gpui_kit::test]
    fn sidebar_click_switches_the_content_pane(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in);
        app.wait_until(cx, "the inbox shows the first sync", |window| {
            window.try_find("inbox-loading").is_some()
        });

        cx.update_window(app.window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("page-title").label(), Some("Inbox"));
            assert!(window.find("sync-status").visible());
            assert!(window.find("player-bar").visible());
            assert!(window.find("podcasts-empty").visible());

            window
                .within("sidebar")
                .click(shell::sidebar::page_element_id(Route::Downloads), cx);
            assert_eq!(window.find("page-title").label(), Some("Downloads"));

            window
                .within("sidebar")
                .click(shell::sidebar::page_element_id(Route::Settings), cx);
            assert_eq!(window.find("page-title").label(), Some("Settings"));
        })
        .expect("the window is still open");
    }
}
