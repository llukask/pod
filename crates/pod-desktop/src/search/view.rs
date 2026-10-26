//! The sidebar field and its results pane (wireframe D3).
//!
//! Typing terms searches the directory after a pause; Enter searches at
//! once. A URL skips the directory and offers "Subscribe to this feed",
//! which, like Subscribe on a result, runs the offline subscribe path
//! (spec 5.5): the show is in the sidebar before any request goes out.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, px,
};
use pod_protocol::{DirectoryResult, NormalizedFeedUrl, PodcastId, normalize_feed_url};
use tokio::task::JoinHandle;

use super::{
    Query, SearchError, classify, feed_url_problem, is_subscribed, load_artwork_files,
    load_subscribed,
};
use crate::reconcile::Reconciler;
use crate::runtime::Background;
use crate::shell::artwork::artwork;
use crate::store::{Store, Table, live};
use crate::sync::SyncHandle;
use crate::sync::engine::SyncEngine;
use crate::sync::subscribe::{self, DirectoryHint};

/// How long typing must pause before the directory is asked, as on Android
/// (wireframe M4).
const DEBOUNCE: Duration = Duration::from_millis(300);

gpui_kit::actions!(search, [FocusSearch]);

/// Binds `Ctrl+K` everywhere in the window.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-k", FocusSearch, None)]);
}

/// Emitted when the field holds something to show results for, so the
/// window switches to the results pane.
#[derive(Debug)]
pub struct ShowResults;

/// What the results pane shows below its heading.
#[derive(Debug)]
enum Pane {
    /// The field is empty, or holds a URL that doesn't normalize (its
    /// problem shows under the field).
    Hint,
    /// A URL: one row to subscribe to it.
    Feed(NormalizedFeedUrl),
    Found {
        term: String,
        results: Vec<DirectoryResult>,
    },
    Failed {
        term: String,
        error: SearchError,
    },
}

pub struct SearchView {
    input: Entity<InputState>,
    pane: Pane,
    /// The term of the search in flight. The pane keeps the last results
    /// until its answer replaces them, so typing doesn't blank the grid.
    searching: Option<String>,
    /// Shown under the field: a URL that doesn't normalize, or a subscribe
    /// the local store refused.
    problem: Option<SharedString>,
    /// The shows the user follows, from a live query.
    subscribed: HashSet<PodcastId>,
    /// Cached artwork files by URL, from a live query.
    artwork_files: HashMap<String, String>,
    /// The search in flight. Replacing or dropping it cancels the request.
    search: Option<Task<()>>,
    _input_events: Subscription,
    _subscribed_query: Task<()>,
    _artwork_query: Task<()>,
}

impl std::fmt::Debug for SearchView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchView")
            .field("pane", &self.pane)
            .field("searching", &self.searching)
            .field("problem", &self.problem)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ShowResults> for SearchView {}

impl SearchView {
    /// Requires the [`Store`], [`SyncHandle`], [`SyncEngine`],
    /// [`Reconciler`] and [`Background`] globals.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search or paste feed URL"));
        let input_events = cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            InputEvent::Change => this.changed(cx),
            InputEvent::PressEnter { .. } => this.submit(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        Self {
            input,
            pane: Pane::Hint,
            searching: None,
            problem: None,
            subscribed: HashSet::new(),
            artwork_files: HashMap::new(),
            search: None,
            _input_events: input_events,
            _subscribed_query: live::observe_tables(
                cx,
                Table::Subscription.into(),
                load_subscribed,
                |this: &mut Self, subscribed, _| this.subscribed = subscribed,
            ),
            _artwork_query: live::observe_tables(
                cx,
                Table::Artwork.into(),
                load_artwork_files,
                |this: &mut Self, files, _| this.artwork_files = files,
            ),
        }
    }

    /// Shows `pane`, and asks the artwork cache for its results' images:
    /// the grid shows them once they are cached.
    fn show(&mut self, pane: Pane, cx: &mut Context<Self>) {
        let urls = match &pane {
            Pane::Found { results, .. } => results
                .iter()
                .filter_map(|result| result.artwork_url.clone())
                .collect(),
            Pane::Hint | Pane::Feed(_) | Pane::Failed { .. } => Vec::new(),
        };
        cx.global::<Reconciler>().want_transient(urls);
        self.pane = pane;
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn query(&self, cx: &App) -> Query {
        classify(&self.input.read(cx).value())
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.search = None;
        self.searching = None;
        self.problem = None;
        match self.query(cx) {
            Query::Empty => self.show(Pane::Hint, cx),
            Query::Feed(Ok(url)) => self.show(Pane::Feed(url), cx),
            Query::Feed(Err(error)) => {
                self.show(Pane::Hint, cx);
                self.problem = Some(feed_url_problem(&error).into());
            }
            Query::Terms(term) => self.start_search(term, DEBOUNCE, cx),
        }
        if !matches!(self.pane, Pane::Hint) || self.searching.is_some() {
            cx.emit(ShowResults);
        }
        cx.notify();
    }

    /// Enter: subscribes to a URL, or searches now instead of after the
    /// pause.
    fn submit(&mut self, cx: &mut Context<Self>) {
        match self.query(cx) {
            Query::Feed(Ok(url)) => self.subscribe(url, None, cx),
            Query::Terms(term) => {
                self.start_search(term, Duration::ZERO, cx);
                cx.emit(ShowResults);
                cx.notify();
            }
            Query::Empty | Query::Feed(Err(_)) => {}
        }
    }

    // TODO: search again when the connection returns while the pane shows
    // the offline message; until then the user retypes or presses Enter.
    // TODO: clear the field and results on logout, so the next account
    // doesn't see the last one's search.
    fn start_search(&mut self, term: String, delay: Duration, cx: &mut Context<Self>) {
        let store = cx.global::<Store>().clone();
        let engine = cx.global::<SyncEngine>().clone();
        self.searching = Some(term.clone());
        let request = term.clone();
        let job = AbortOnDrop(cx.global::<Background>().spawn(async move {
            tokio::time::sleep(delay).await;
            super::search(&store, engine.http(), engine.network().connected, request).await
        }));
        self.search = Some(cx.spawn(async move |this, cx| {
            let mut job = job;
            let result = (&mut job.0).await;
            let _ = this.update(cx, |this, cx| {
                this.searching = None;
                let pane = match result {
                    Ok(Ok(results)) => Pane::Found { term, results },
                    Ok(Err(error)) => Pane::Failed { term, error },
                    Err(error) => Pane::Failed {
                        term,
                        error: SearchError::Local(error.into()),
                    },
                };
                this.show(pane, cx);
                cx.notify();
            });
        }));
    }

    fn subscribe_to_result(&mut self, result: &DirectoryResult, cx: &mut Context<Self>) {
        match normalize_feed_url(&result.feed_url) {
            Ok(url) => self.subscribe(url, Some(DirectoryHint::from(result)), cx),
            Err(error) => {
                self.problem = Some(feed_url_problem(&error).into());
                cx.notify();
            }
        }
    }

    /// The offline subscribe path (spec 5.5). The sidebar and the
    /// Subscribed marks follow the local write through their live queries.
    fn subscribe(
        &mut self,
        url: NormalizedFeedUrl,
        hint: Option<DirectoryHint>,
        cx: &mut Context<Self>,
    ) {
        let store = cx.global::<Store>().clone();
        let sync = cx.global::<SyncHandle>().clone();
        let job = cx.global::<Background>().spawn(async move {
            subscribe::subscribe(&store, &sync, &url, hint)
                .await
                .map_err(|error| format!("{error:#}"))
        });
        cx.spawn(async move |this, cx| {
            let error = match job.await {
                Ok(Ok(_)) => return,
                Ok(Err(error)) => error,
                Err(error) => error.to_string(),
            };
            tracing::error!(error, "subscribing failed locally");
            let _ = this.update(cx, |this, cx| {
                this.problem = Some(format!("Couldn't add the show: {error}").into());
                cx.notify();
            });
        })
        .detach();
    }

    /// The field and its problem, for the sidebar header.
    pub fn render_field(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(
                Input::new(&self.input)
                    .id("search-field")
                    .prefix(IconName::Search)
                    .cleanable(true),
            )
            .when_some(self.problem.clone(), |field, problem| {
                field.child(
                    div()
                        .id("search-problem")
                        .aria_label(problem.clone())
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(problem),
                )
            })
    }

    fn heading(&self) -> (String, Option<&'static str>) {
        if let Some(term) = &self.searching {
            return (format!("Searching for \u{201c}{term}\u{201d}…"), None);
        }
        match &self.pane {
            Pane::Hint => ("Search".to_owned(), None),
            Pane::Feed(_) => ("Add a feed".to_owned(), None),
            Pane::Found { term, .. } | Pane::Failed { term, .. } => (
                format!("Results for \u{201c}{term}\u{201d}"),
                Some("from the Apple Podcasts directory"),
            ),
        }
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.pane {
            Pane::Hint => message(
                "Type a show's name to search the directory, or paste its feed URL.",
                cx,
            ),
            Pane::Failed { error, .. } => message(error.to_string(), cx),
            Pane::Found { term, results } if results.is_empty() => {
                message(format!("No shows found for \u{201c}{term}\u{201d}."), cx)
            }
            Pane::Found { results, .. } => h_flex()
                .flex_wrap()
                .items_start()
                .gap_4()
                .children(
                    results
                        .iter()
                        .enumerate()
                        .map(|(index, result)| self.render_result(index, result, cx))
                        .collect::<Vec<_>>(),
                )
                .into_any_element(),
            Pane::Feed(url) => {
                let subscribed = self.subscribed.contains(&PodcastId::from_feed_url(url));
                let target = url.clone();
                h_flex()
                    .id("search-feed")
                    .test_support()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Subscribe to this feed"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(url.feed_url().to_owned()),
                            ),
                    )
                    .child(subscribe_button(
                        "search-feed".into(),
                        subscribed,
                        cx.listener(move |this, _, _, cx| {
                            this.subscribe(target.clone(), None, cx);
                        }),
                    ))
                    .into_any_element()
            }
        }
    }

    fn render_result(
        &self,
        index: usize,
        result: &DirectoryResult,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let subscribed = is_subscribed(result, &self.subscribed);
        let title = SharedString::from(result.title.clone());
        let target = result.clone();
        v_flex()
            .id(SharedString::from(format!("search-result-{index}")))
            .aria_label(title.clone())
            .test_support()
            .w(px(168.))
            .gap_1()
            .child(artwork(
                SharedString::from(format!("search-result-{index}-artwork")),
                result
                    .artwork_url
                    .as_ref()
                    .and_then(|url| self.artwork_files.get(url))
                    .map(String::as_str),
                px(168.),
                cx,
            ))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_clamp(2)
                    .child(title),
            )
            .children(result.author.clone().map(|author| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(author)
            }))
            .child(subscribe_button(
                format!("search-result-{index}").into(),
                subscribed,
                cx.listener(move |this, _, _, cx| this.subscribe_to_result(&target, cx)),
            ))
            .into_any_element()
    }
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (heading, source) = self.heading();
        let heading = SharedString::from(heading);
        v_flex()
            .id("content")
            .flex_1()
            .min_w_0()
            .h_full()
            .p_6()
            .gap_4()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .id("page-title")
                            .aria_label(heading.clone())
                            .test_support()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(heading),
                    )
                    .children(source.map(|source| {
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(source)
                    })),
            )
            .child(self.render_body(cx))
    }
}

/// "Subscribe", or a disabled "Subscribed" with a check once the show is
/// followed. Their ids differ (`<prefix>-subscribe`, `<prefix>-subscribed`)
/// so tests can tell them apart.
fn subscribe_button(
    prefix: SharedString,
    subscribed: bool,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    if subscribed {
        Button::new(SharedString::from(format!("{prefix}-subscribed")))
            .ghost()
            .icon(IconName::Check)
            .label("Subscribed")
            .disabled(true)
    } else {
        Button::new(SharedString::from(format!("{prefix}-subscribe")))
            .outline()
            .label("Subscribe")
            .on_click(on_click)
    }
}

fn message(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    let text = text.into();
    div()
        .id("search-message")
        .aria_label(text.clone())
        .test_support()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// Aborts the tokio task when the search it belongs to is replaced, so a
/// stale request stops instead of finishing unseen.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse as _, Response};
    use color_eyre::eyre::WrapErr as _;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use pod_protocol::{DirectorySearchResponse, ErrorBody, ErrorCode};
    use rusqlite::{Transaction, params};
    use tokio::sync::{oneshot, watch};

    use super::*;
    use crate::app::tests::{TestApp, signed_in};
    use crate::download::tests::{ensure_server, image_requests};
    use crate::net::NetworkState;
    use crate::reconcile::Reconciler;
    use crate::sync::engine::EngineState;
    use crate::test_port::bind_test_port;

    // ==========================================================================
    // A stub server
    // ==========================================================================

    const SYNTAX_FEED: &str = "https://feeds.syntax.example.com/rss";
    /// An old address of [`FOLLOWED_FEED`]; the directory still lists it.
    const OLD_ALIAS_FEED: &str = "https://old.example.com/syntax-error.xml";
    const FOLLOWED_FEED: &str = "https://new.example.com/syntax-error.xml";

    fn id_of(feed_url: &str) -> PodcastId {
        PodcastId::from_feed_url(&normalize_feed_url(feed_url).expect("the URL is valid"))
    }

    /// Answers `q=syntax` with two shows and `q=broken` with
    /// `upstream_unavailable`. Holds every sync until [`Stub::release`],
    /// then answers it with `503`. Records what reached it.
    struct Stub {
        requests: Arc<Mutex<Vec<String>>>,
        release: watch::Sender<bool>,
        _stop: oneshot::Sender<()>,
    }

    impl Stub {
        /// Serves on a port from [`bind_test_port`] (`POD_TEST_SEARCH_PORT`
        /// overrides the choice) and points the seeded account at the stub.
        fn start(app: &TestApp) -> Stub {
            let listener = bind_test_port("POD_TEST_SEARCH_PORT");
            let base_url = format!(
                "http://{}",
                listener
                    .local_addr()
                    .expect("a bound listener has an address")
            );
            app.runtime
                .block_on(app.store.write(move |transaction| {
                    transaction
                        .execute("update account set server_url = ?1", [base_url])
                        .context("point the account at the stub")
                }))
                .expect("the account update is written");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let (release, released) = watch::channel(false);
            let recorded = requests.clone();
            let router = axum::Router::new().fallback(move |request: axum::extract::Request| {
                let recorded = recorded.clone();
                let mut released = released.clone();
                async move {
                    let uri = request.uri().to_string();
                    let headers = request.headers().clone();
                    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                        .await
                        .expect("the body arrives");
                    recorded
                        .lock()
                        .expect("not poisoned")
                        .push(format!("{uri} {}", String::from_utf8_lossy(&body)));
                    if uri.starts_with("/v1/sync") {
                        let _ = released.wait_for(|released| *released).await;
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    search_answer(&uri, &headers)
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
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("the stub runs");
            });
            Stub {
                requests,
                release,
                _stop: stop,
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("not poisoned").clone()
        }

        fn release(&self) {
            self.release.send_replace(true);
        }
    }

    impl Drop for Stub {
        /// A held sync would keep the graceful shutdown waiting.
        fn drop(&mut self) {
            self.release();
        }
    }

    fn search_answer(uri: &str, headers: &HeaderMap) -> Response {
        let authorized = headers
            .get("authorization")
            .is_some_and(|value| value == "Bearer secret");
        if !authorized {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(ErrorBody::new(ErrorCode::Unauthenticated, "no token")),
            )
                .into_response();
        }
        if uri == "/v1/directory/search?q=broken" {
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(ErrorBody::new(
                    ErrorCode::UpstreamUnavailable,
                    "the directory failed",
                )),
            )
                .into_response();
        }
        assert_eq!(
            uri, "/v1/directory/search?q=syntax",
            "the only other search"
        );
        axum::Json(DirectorySearchResponse {
            results: vec![
                DirectoryResult {
                    title: "Syntax".to_owned(),
                    author: Some("Wes Bos & Scott Tolinski".to_owned()),
                    feed_url: SYNTAX_FEED.to_owned(),
                    artwork_url: Some(syntax_artwork()),
                    podcast_id: id_of(SYNTAX_FEED),
                },
                DirectoryResult {
                    title: "Syntax Error".to_owned(),
                    author: None,
                    feed_url: OLD_ALIAS_FEED.to_owned(),
                    artwork_url: None,
                    podcast_id: id_of(FOLLOWED_FEED),
                },
            ],
        })
        .into_response()
    }

    fn syntax_artwork() -> String {
        format!("{}/image/search-syntax", ensure_server())
    }

    /// Signed in and following the show behind [`OLD_ALIAS_FEED`] under its
    /// new address. [`Stub::start`] points the account at the stub.
    fn signed_in_on_stub(transaction: &Transaction<'_>) -> color_eyre::Result<()> {
        signed_in(transaction)?;
        let followed = id_of(FOLLOWED_FEED).to_string();
        transaction
            .execute(
                "insert into podcast (id, feed_url, status, version, title)
                 values (?1, ?2, 'ok', 3, 'Syntax Error')",
                params![followed, FOLLOWED_FEED],
            )
            .context("insert the followed podcast")?;
        transaction
            .execute(
                "insert into subscription
                     (podcast_id, feed_url, subscribed, subscribed_at, subscribed_dev)
                 values (?1, ?2, 1, 0, '019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d')",
                params![followed, FOLLOWED_FEED],
            )
            .context("insert the subscription")?;
        Ok(())
    }

    fn label(app: &TestApp, cx: &mut TestAppContext, id: &'static str) -> Option<String> {
        cx.update_window(app.window, |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find(id)
                .and_then(|element| element.label().map(str::to_owned))
        })
        .expect("the window is still open")
    }

    fn in_sidebar(window: &mut Window, podcast_id: PodcastId) -> bool {
        window
            .within("sidebar")
            .try_find(format!("podcast-unplayed-{podcast_id}"))
            .is_some()
    }

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // ==========================================================================
    // Tests
    // ==========================================================================

    #[gpui_kit::test]
    fn subscribing_from_a_result_shows_the_show_before_the_sync_answers(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in_on_stub);
        let stub = Stub::start(&app);
        app.wait_until(cx, "the app shows", |window| {
            window.try_find("player-bar").is_some()
        });

        cx.update_window(app.window, |_, window, cx| {
            window.click("search-field", cx);
            window.input("syntax", cx);
        })
        .expect("the window is still open");
        // The marks come from the view's subscription query, which answers
        // apart from the search request.
        app.wait_until(
            cx,
            "the results show, with an old alias of a followed show marked",
            |window| window.try_find("search-result-1-subscribed").is_some(),
        );
        assert_eq!(
            label(&app, cx, "page-title").as_deref(),
            Some("Results for \u{201c}syntax\u{201d}")
        );
        let searches: Vec<_> = stub
            .requests()
            .into_iter()
            .filter(|request| request.starts_with("/v1/directory"))
            .collect();
        assert_eq!(
            searches,
            ["/v1/directory/search?q=syntax "],
            "one request after the pause, not one per keystroke"
        );

        cx.update_window(app.window, |_, window, cx| {
            assert_eq!(window.find("search-result-0").label(), Some("Syntax"));
            window.click("search-result-0-subscribe", cx);
        })
        .expect("the window is still open");

        // The driver isn't running in UI tests, so the test starts the sync
        // the subscribe asked for.
        wait_for("the subscribe asks for a sync", || {
            app.sync.wanted().borrow().is_some()
        });
        let engine = app.engine.clone();
        let sync = app.background.spawn(async move { engine.sync().await });
        wait_for("the stub holds the sync carrying the subscribe", || {
            stub.requests()
                .iter()
                .any(|request| request.starts_with("/v1/sync") && request.contains(SYNTAX_FEED))
        });

        let syntax = id_of(SYNTAX_FEED);
        app.wait_until(
            cx,
            "the show is in the sidebar and marked followed",
            |window| {
                in_sidebar(window, syntax)
                    && window.try_find("search-result-0-subscribed").is_some()
            },
        );
        assert!(!sync.is_finished(), "the sync hasn't been answered");
        let local_title: Option<String> = app.read(move |connection| {
            connection
                .query_row(
                    "select local_title from podcast where id = ?1",
                    [syntax.to_string()],
                    |row| row.get(0),
                )
                .context("read the placeholder")
        });
        assert_eq!(local_title.as_deref(), Some("Syntax"), "the hint's title");

        stub.release();
        let state = app
            .runtime
            .block_on(sync)
            .expect("the sync task runs")
            .expect("the local store works");
        assert!(matches!(state, EngineState::BackingOff { .. }));
    }

    /// Results arrive with their artwork's URLs; a pass fetches the images
    /// into the cache, and the grid shows the cached files.
    #[gpui_kit::test]
    fn result_artwork_is_fetched_into_the_cache_and_shown_from_it(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in_on_stub);
        let _stub = Stub::start(&app);
        app.wait_until(cx, "the app shows", |window| {
            window.try_find("player-bar").is_some()
        });
        cx.update_window(app.window, |_, window, cx| {
            window.click("search-field", cx);
            window.input("syntax", cx);
        })
        .expect("the window is still open");
        app.wait_until(cx, "the results show", |window| {
            window.try_find("search-result-1").is_some()
        });

        let reconciler = cx.update(|cx| cx.global::<Reconciler>().clone());
        app.runtime
            .block_on(reconciler.pass())
            .expect("the pass runs");
        let expected = reconciler
            .artwork_root()
            .join(crate::reconcile::artwork::file_name(&syntax_artwork()))
            .display()
            .to_string();
        app.wait_until(cx, "the first result shows the cached file", |window| {
            window
                .try_find("search-result-0-artwork")
                .is_some_and(|element| element.value() == Some(expected.as_str()))
        });
        cx.update_window(app.window, |_, window, _| {
            assert_eq!(
                window.find("search-result-1-artwork").value(),
                Some(""),
                "a result without artwork keeps the placeholder"
            );
        })
        .expect("the window is still open");
        assert_eq!(image_requests("search-syntax"), 1);
    }

    #[gpui_kit::test]
    fn offline_search_says_so_and_a_pasted_url_still_subscribes(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in_on_stub);
        app.network.send_replace(NetworkState {
            connected: false,
            metered: false,
        });
        app.wait_until(cx, "the app shows", |window| {
            window.try_find("player-bar").is_some()
        });

        cx.update_window(app.window, |_, window, cx| {
            window.press("ctrl-k", cx);
            window.input("syntax", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(
            cx,
            "search-message",
            "Search needs a connection. You can still add a show by pasting its feed URL.",
        );

        cx.update_window(app.window, |_, window, cx| {
            window.press("ctrl-a", cx);
            window.input("feeds.example.com/show.xml", cx);
        })
        .expect("the window is still open");
        // The field's change event reaches the view once the update ends.
        cx.update_window(app.window, |_, window, cx| {
            window.click("search-feed-subscribe", cx);
        })
        .expect("the window is still open");
        let pasted = id_of("https://feeds.example.com/show.xml");
        app.wait_until(cx, "the pasted feed is in the sidebar", |window| {
            in_sidebar(window, pasted) && window.try_find("search-feed-subscribed").is_some()
        });
    }

    #[gpui_kit::test]
    fn problems_show_in_plain_words(cx: &mut TestAppContext) {
        let app = TestApp::open(cx, signed_in_on_stub);
        let _stub = Stub::start(&app);
        app.wait_until(cx, "the app shows", |window| {
            window.try_find("player-bar").is_some()
        });

        cx.update_window(app.window, |_, window, cx| {
            window.click("search-field", cx);
            window.input("feed://example.com/rss", cx);
        })
        .expect("the window is still open");
        assert_eq!(
            label(&app, cx, "search-problem").as_deref(),
            Some("Feed addresses start with http:// or https://.")
        );

        cx.update_window(app.window, |_, window, cx| {
            window.press("ctrl-a", cx);
            window.input("broken", cx);
            window.press("enter", cx);
        })
        .expect("the window is still open");
        app.wait_for_label(
            cx,
            "search-message",
            "The podcast directory isn't answering right now. Try again in a moment.",
        );
        assert!(label(&app, cx, "search-problem").is_none());
    }
}
