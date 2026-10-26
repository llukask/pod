//! The root view: which page the window shows, and the shell around it
//! (wireframe D1).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::{ActiveTheme as _, TitleBar, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Result, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, WindowBounds, WindowOptions, div,
    px, size,
};
use pod_protocol::{MutationId, PodcastId, Timestamp};

use crate::account::{self, SignInForm};
use crate::downloads::view::{DownloadList, DownloadsView, OpenSettings};
use crate::inbox::view::InboxView;
use crate::inbox::{self, SidebarPodcast};
use crate::player;
use crate::podcast::view::PodcastView;
use crate::search::view::{FocusSearch, SearchView, ShowResults};
use crate::shell;
use crate::shell::player_bar::PlayerBar;
use crate::shell::sync_status::{self, SyncCounts, SyncNow};
use crate::store::{Table, live};
use crate::sync::engine::{EngineState, SyncEngine};
use crate::sync::session::{Account, load_account};
use crate::sync::subscribe::{self, SubscribeFailure};

/// The page selected in the sidebar. The list and detail panes show its
/// content; the title bar and player bar stay the same for every route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Inbox,
    Downloads,
    Settings,
    Podcast(PodcastId),
    /// The sidebar field's results (wireframe D3).
    Search,
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
    downloads: Entity<DownloadsView>,
    /// The page of the podcast the route names, made on the first render
    /// of that route.
    podcast: Option<Entity<PodcastView>>,
    player_bar: Entity<PlayerBar>,
    /// The sidebar field and its results pane.
    search: Entity<SearchView>,
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
    /// Where subscribe failures are shown.
    window: AnyWindowHandle,
    /// The subscribe failures already in the log, `None` until the first
    /// query result arrives.
    seen_failures: Option<HashSet<MutationId>>,
    _failures_query: Task<()>,
    _podcasts_query: Task<()>,
    _account_query: Task<()>,
    _sync_state: Task<()>,
    _sync_counts_query: Task<()>,
    _sync_status_ticker: Task<()>,
    _search_events: Subscription,
    _downloads_events: Subscription,
}

impl PodApp {
    /// Requires the [`crate::store::Store`], [`crate::sync::SyncHandle`],
    /// [`SyncEngine`], [`crate::sync::session::Session`] and
    /// [`crate::runtime::Background`], [`crate::reconcile::Reconciler`] and
    /// [`player::PlayerHandle`] globals.
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
        let search = cx.new(|cx| SearchView::new(window, cx));
        let search_events = cx.subscribe(&search, |this, _, _: &ShowResults, cx| {
            this.navigate(Route::Search, cx);
        });
        let download_list = cx.new(DownloadList::new);
        let downloads = cx.new(|cx| DownloadsView::new(download_list.clone(), cx));
        let downloads_events = cx.subscribe(&downloads, |this, _, _: &OpenSettings, cx| {
            this.navigate(Route::Settings, cx);
        });
        Self {
            route: Route::Inbox,
            inbox: cx.new(|cx| InboxView::new(download_list, window, cx)),
            downloads,
            podcast: None,
            player_bar: cx.new(PlayerBar::new),
            search,
            podcasts: Vec::new(),
            gate: Gate::Loading,
            sign_in: None,
            sync_state: cx.global::<SyncEngine>().state().borrow().clone(),
            sync_counts: SyncCounts::default(),
            sync_status: SharedString::default(),
            window: window.window_handle(),
            seen_failures: None,
            _failures_query: live::observe_tables(
                cx,
                Table::RejectedMutation.into(),
                subscribe::load_failures,
                Self::show_new_failures,
            ),
            _podcasts_query: live::observe_tables(
                cx,
                inbox::tables(),
                |connection| inbox::load_podcasts(connection, Timestamp::now()),
                |this: &mut Self, podcasts: Vec<SidebarPodcast>, _| {
                    // An unsubscribe, here or synced from another device,
                    // closes the show's page along with its sidebar entry.
                    if let Route::Podcast(id) = this.route
                        && !podcasts.iter().any(|podcast| podcast.id == id)
                    {
                        this.route = Route::Inbox;
                        this.podcast = None;
                    }
                    this.podcasts = podcasts;
                },
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
            _search_events: search_events,
            _downloads_events: downloads_events,
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

    /// Shows "Couldn't add <url>: <reason>" (spec 5.5) for each subscribe
    /// the server rejected since the window opened. The first result only
    /// records the failures the log already holds: they were shown when
    /// they happened.
    fn show_new_failures(&mut self, failures: Vec<SubscribeFailure>, cx: &mut Context<Self>) {
        let Some(seen) = &mut self.seen_failures else {
            self.seen_failures = Some(failures.iter().map(|f| f.mutation_id).collect());
            return;
        };
        let fresh: Vec<String> = failures
            .into_iter()
            .filter(|failure| seen.insert(failure.mutation_id))
            .map(|failure| failure.message)
            .collect();
        if fresh.is_empty() {
            return;
        }
        let window = self.window;
        // This runs inside an update of the app entity; pushing touches the
        // window's root, so it waits until the update ends.
        cx.defer(move |cx| {
            let pushed = window.update(cx, |_, window, cx| {
                for message in fresh {
                    window.push_notification(failure_notification(message), cx);
                }
            });
            if pushed.is_err() {
                tracing::warn!("the window closed before a subscribe failure showed");
            }
        });
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            if !matches!(route, Route::Podcast(_)) {
                self.podcast = None;
            }
            cx.notify();
        }
    }

    /// Podcast `id`'s page, made fresh when the route moved to another
    /// podcast.
    fn podcast_page(
        &mut self,
        id: PodcastId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PodcastView> {
        match &self.podcast {
            Some(page) if page.read(cx).podcast_id() == id => page.clone(),
            _ => {
                let page = cx.new(|cx| PodcastView::new(id, window, cx));
                self.podcast = Some(page.clone());
                page
            }
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
        // Every page but Settings, which needs the account.
        let page = match self.route {
            Route::Inbox => Some(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.inbox.clone())
                    .into_any_element(),
            ),
            Route::Search => Some(self.search.clone().into_any_element()),
            Route::Podcast(id) => Some(self.podcast_page(id, window, cx).into_any_element()),
            Route::Downloads => Some(self.downloads.clone().into_any_element()),
            Route::Settings => None,
        };
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
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search
                    .update(cx, |search, cx| search.focus(window, cx));
            }))
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
                    .child(shell::sidebar::render(
                        self.route,
                        &self.podcasts,
                        &self.search,
                        cx,
                    ))
                    .child(page.unwrap_or_else(|| {
                        shell::content::settings(account, &self.sync_status, self.sync_counts, cx)
                    })),
            )
            .child(self.player_bar.clone())
    }
}

/// A failed subscribe, which stays until dismissed: the show the user just
/// added has vanished from the sidebar, and this says why.
fn failure_notification(message: String) -> Notification {
    Notification::new()
        .with_type(NotificationType::Error)
        .autohide(false)
        .content(move |_, _, _| {
            div()
                .id("subscribe-failure")
                .aria_label(message.clone())
                .test_support()
                .child(message.clone())
                .into_any_element()
        })
}

/// Registers the key bindings of the window's pages and the player's
/// actions.
pub fn init(cx: &mut App) {
    inbox::view::init(cx);
    crate::podcast::view::init(cx);
    sync_status::init(cx);
    player::init(cx);
    crate::search::view::init(cx);
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
    use crate::player::mpv::{Command, Engine, Event};
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
        /// What the player asked mpv to do.
        pub mpv_commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
        /// Plays mpv's part towards the player.
        pub mpv_events: tokio::sync::mpsc::UnboundedSender<Event>,
        /// The OS connectivity signal the engine and search read.
        pub network: watch::Sender<NetworkState>,
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
            let (network_sender, network) = watch::channel(NetworkState::ASSUMED);
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
            let reconciler =
                Reconciler::new(downloader, dir.path().join("artwork"), |_| Ok(1 << 40));
            let session = Session::new(
                store.clone(),
                sync.clone(),
                engine.clone(),
                reconciler.clone(),
            );
            let (mpv, mpv_commands, mpv_events) = Engine::scripted();
            cx.update(|cx| {
                gpui_kit::init(cx);
                // A click aims at the bounds of the last rendered frame. A
                // dialog still sliding in on a busy CPU moves between that
                // frame and the click, and the click misses its button.
                cx.set_reduce_motion(true);
                init(cx);
                cx.set_global(store.clone());
                cx.set_global(sync.clone());
                cx.set_global(engine.clone());
                cx.set_global(session);
                cx.set_global(background.clone());
                cx.set_global(reconciler);
                player::install(mpv, cx);
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
                mpv_commands,
                mpv_events,
                network: network_sender,
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

        /// Renders frames until the element `id` shows with `label`. Each
        /// live query delivers on its own, so one query's rows on screen
        /// don't mean another's have arrived: wait for every element a test
        /// reads, not only the first.
        pub fn wait_for_label(&self, cx: &mut TestAppContext, id: &str, label: &str) {
            self.wait_until(cx, &format!("{id} reads {label:?}"), |window| {
                window
                    .try_find(id.to_owned())
                    .is_some_and(|element| element.label() == Some(label))
            });
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

    /// A signed-in account and nothing else. Its server is a port nothing
    /// listens on; tests with a stub server point the account at it.
    pub(crate) fn signed_in(transaction: &Transaction<'_>) -> EyreResult<()> {
        use color_eyre::eyre::WrapErr as _;
        transaction
            .execute(
                "insert into account (id, server_url, user_id, username, device_id, token)
                 values (1, 'http://127.0.0.1:9', '019a3f10-8c2e-7a41-9b3d-ffffffffffff',
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

    /// Logs a subscribe to `feed_url` as rejected with `invalid_feed_url`,
    /// as the sync apply step does.
    fn reject_subscribe(
        transaction: &Transaction<'_>,
        mutation_id: &str,
        feed_url: &str,
    ) -> EyreResult<()> {
        use color_eyre::eyre::WrapErr as _;
        let payload = serde_json::json!({
            "type": "subscription.set", "id": mutation_id, "at": "2026-10-24T08:00:00Z",
            "dev": "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d",
            "podcast_id": "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c",
            "feed_url": feed_url, "subscribed": true
        });
        transaction
            .execute(
                "insert into rejected_mutation (mutation_id, payload, reason, rejected_at)
                 values (?1, ?2, 'invalid_feed_url', 0)",
                rusqlite::params![mutation_id, payload.to_string()],
            )
            .context("log the rejection")?;
        Ok(())
    }

    #[gpui_kit::test]
    fn a_subscribe_the_server_rejects_shows_a_notification(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, |transaction| {
            signed_in(transaction)?;
            reject_subscribe(
                transaction,
                "019a3f10-9999-7a41-9b3d-000000000001",
                "https://earlier.example.com/feed.xml",
            )
        });
        app.wait_until(cx, "the inbox shows the first sync", |window| {
            window.try_find("inbox-loading").is_some()
        });
        app.runtime
            .block_on(app.store.write(|transaction| {
                reject_subscribe(
                    transaction,
                    "019a3f10-9999-7a41-9b3d-000000000002",
                    "https://alice:secret@10.0.0.1/feed.xml",
                )
            }))
            .expect("the rejection is logged");

        app.wait_until(cx, "the failure shows", |window| {
            window.try_find("subscribe-failure").is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            window.render_frame(cx);
            let shown: Vec<_> = window
                .find_all("subscribe-failure")
                .into_iter()
                .map(|note| note.label().map(str::to_owned))
                .collect();
            assert_eq!(
                shown,
                [Some(
                    "Couldn't add https://10.0.0.1/feed.xml: \
                     the server won't fetch from this address"
                        .to_owned()
                )],
                "only the new failure shows, without the feed's credentials"
            );
        })
        .expect("the window is still open");
    }
}
