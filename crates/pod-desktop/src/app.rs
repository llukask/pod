//! The root view: which page the window shows, and the shell around it
//! (wireframe D1).

use gpui_kit::component::{ActiveTheme as _, TitleBar, h_flex, v_flex};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, IntoElement,
    ParentElement as _, Render, Result, Styled as _, Task, Window, WindowBounds, WindowOptions, px,
    size,
};
use pod_protocol::{PodcastId, Timestamp};

use crate::inbox::view::InboxView;
use crate::inbox::{self, SidebarPodcast};
use crate::shell;
use crate::store::live;

/// The page selected in the sidebar. The list and detail panes show its
/// content; the title bar and player bar stay the same for every route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Inbox,
    Downloads,
    Settings,
    Podcast(PodcastId),
}

/// Owns navigation state for the main window, and the pages that keep
/// state across navigation.
#[derive(Debug)]
pub struct PodApp {
    route: Route,
    inbox: Entity<InboxView>,
    /// The sidebar's Podcasts group, from a live query.
    podcasts: Vec<SidebarPodcast>,
    _podcasts_query: Task<()>,
}

impl PodApp {
    /// Requires the [`crate::store::Store`] and [`crate::sync::SyncHandle`]
    /// globals.
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            route: Route::Inbox,
            inbox: cx.new(|cx| InboxView::new(window, cx)),
            podcasts: Vec::new(),
            _podcasts_query: live::observe_tables(
                cx,
                inbox::tables(),
                |connection| inbox::load_podcasts(connection, Timestamp::now()),
                |this: &mut Self, podcasts, _| this.podcasts = podcasts,
            ),
        }
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            cx.notify();
        }
    }
}

impl Render for PodApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(shell::title_bar::render(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(shell::sidebar::render(self.route, &self.podcasts, cx))
                    .child(shell::content::render(self.route, &self.inbox, cx)),
            )
            .child(shell::player_bar::render(cx))
    }
}

/// Registers the key bindings of the window's pages.
pub fn init(cx: &mut App) {
    inbox::view::init(cx);
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

    use super::*;
    use crate::store::Store;
    use crate::store::tests::scratch_dir;
    use crate::sync::SyncHandle;

    /// The main window over a scratch store.
    pub(crate) struct TestApp {
        _dir: TempDir,
        pub store: Store,
        /// Runs store calls from the test thread, outside the foreground
        /// executor.
        pub runtime: tokio::runtime::Runtime,
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
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime builds");
            runtime
                .block_on(store.write(seed))
                .expect("the seed data is written");
            cx.update(|cx| {
                gpui_kit::init(cx);
                init(cx);
                cx.set_global(store.clone());
                cx.set_global(SyncHandle::default());
            });
            let window = cx
                .update(open_main_window)
                .expect("the test platform opens windows");
            TestApp {
                _dir: dir,
                store,
                runtime,
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

    #[gpui_kit::test]
    fn sidebar_click_switches_the_content_pane(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, |_| Ok(()));
        app.wait_until(cx, "the inbox query ran", |window| {
            window.try_find("inbox-empty").is_some()
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
