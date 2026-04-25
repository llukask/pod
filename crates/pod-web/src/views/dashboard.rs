// ==============================================================================
// Dashboard view
// ==============================================================================
//
// The "Subscriptions" tab. Lists the user's subscribed podcasts and offers
// two ways to add a new one:
//   - Apple Podcasts search (debounced, hits iTunes search directly).
//   - Raw RSS URL.
//
// The Inbox tab is rendered by `views::inbox::InboxView` at /inbox.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};

use gloo_timers::callback::Timeout;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

// Debounce timer for the Apple search input. `gloo_timers::Timeout` isn't
// `Send`, so it can't live inside a leptos `StoredValue` (which assumes
// SyncStorage). A thread-local works fine on wasm32 where there's only one
// thread.
thread_local! {
    static SEARCH_DEBOUNCE: RefCell<Option<Timeout>> = const { RefCell::new(None) };
}

use crate::api;
use crate::state::use_app_state;
use crate::util::{format_date, format_date_opt, strip_html};
use crate::views::shared::ImageWithPlaceholder;

const SEARCH_DEBOUNCE_MS: u32 = 350;
const SEARCH_MIN_CHARS: usize = 2;
const SEARCH_LIMIT: u32 = 12;

#[component]
pub fn DashboardView() -> impl IntoView {
    let st = use_app_state();
    st.breadcrumb.set(String::new());

    let show_add = RwSignal::new(false);
    let load_error = RwSignal::new(String::new());
    let loading = RwSignal::new(true);

    let load = move || {
        loading.set(true);
        load_error.set(String::new());
        spawn_local(async move {
            match api::list_podcasts().await {
                Ok(list) => use_app_state().podcasts.set(list),
                Err(e) => load_error.set(e.0),
            }
            loading.set(false);
        });
    };

    Effect::new(move |_| load());

    view! {
        <div class="dashboard-tabs">
            <button class="dashboard-tab active">"Subscriptions"</button>
            <A href="/inbox" attr:class="dashboard-tab">"Inbox"</A>
        </div>
        <div class="dashboard-stack">
            <section>
                <div class="section-label-row">
                    <div class="section-label">"Your Subscriptions"</div>
                    <button
                        class=move || if show_add.get() { "btn btn-secondary btn-sm" } else { "btn btn-primary btn-sm" }
                        on:click=move |_| show_add.update(|v| *v = !*v)>
                        {move || if show_add.get() { "Cancel" } else { "+ Add Podcast" }}
                    </button>
                </div>

                <Show when=move || show_add.get()>
                    <AddPanels on_added=load.clone() />
                </Show>

                <div id="podcast-list">
                    {move || {
                        if loading.get() {
                            view! { <div class="loading-state">"LOADING..."</div> }.into_any()
                        } else if !load_error.get().is_empty() {
                            view! { <div class="error-msg">{load_error.get()}</div> }.into_any()
                        } else if st.podcasts.get().is_empty() {
                            view! {
                                <div class="empty-state">
                                    "No subscriptions yet."<br/>"Add a podcast feed above to get started."
                                </div>
                            }.into_any()
                        } else {
                            view! { <PodcastList /> }.into_any()
                        }
                    }}
                </div>
            </section>
        </div>
    }
}

#[component]
fn PodcastList() -> impl IntoView {
    let podcasts = use_app_state().podcasts;

    view! {
        <ul class="podcast-list">
            <For
                each={move || podcasts.get()}
                key={|p: &pod_model::PodcastWithEpisodeStats| p.id.clone()}
                children={move |p: pod_model::PodcastWithEpisodeStats| {
                    let desc = strip_html(&p.description);
                    let id = p.id.clone();
                    let img = p.image_link.clone();
                    let title = p.title.clone();
                    let last_pub = p.last_publication_date;
                    let desc_for_when = desc.clone();
                    view! {
                        <A href=format!("/podcast/{}", id) attr:class="podcast-card">
                            <ImageWithPlaceholder src=Signal::derive(move || img.clone()) size=80 />
                            <div class="podcast-card-body">
                                <div class="podcast-card-title">{title}</div>
                                <Show when=move || !desc_for_when.is_empty()>
                                    <div class="podcast-card-desc">{desc.clone()}</div>
                                </Show>
                                <div class="podcast-card-meta">
                                    {match last_pub {
                                        Some(d) => format!("Latest: {}", format_date(&d)),
                                        None => "No episodes".to_string(),
                                    }}
                                </div>
                            </div>
                        </A>
                    }
                }}
            />
        </ul>
    }
}

// ------------------------------------------------------------------------------
// "Add Podcast" panels: Apple search + raw RSS URL
// ------------------------------------------------------------------------------

/// Monotonic counter used to drop stale Apple search responses.
static SEARCH_REQ_ID: AtomicU32 = AtomicU32::new(0);

#[component]
fn AddPanels(#[prop()] on_added: impl Fn() + 'static + Clone + Send + Sync) -> impl IntoView {
    let search_query = RwSignal::new(String::new());
    let search_results = RwSignal::new(Vec::<api::AppleSearchResult>::new());
    let search_loading = RwSignal::new(false);
    let search_error = RwSignal::new(String::new());
    let search_has_searched = RwSignal::new(false);
    let subscribing_feed_url = RwSignal::new(String::new());

    let on_added_search = on_added.clone();
    let on_added_url = on_added.clone();

    let trigger_search = move |query: String| {
        SEARCH_DEBOUNCE.with(|cell| cell.borrow_mut().take());
        search_query.set(query.clone());

        if query.chars().count() < SEARCH_MIN_CHARS {
            search_results.set(Vec::new());
            search_loading.set(false);
            search_has_searched.set(false);
            search_error.set(String::new());
            return;
        }

        let q = query.clone();
        let timeout = Timeout::new(SEARCH_DEBOUNCE_MS, move || {
            let req_id = SEARCH_REQ_ID.fetch_add(1, Ordering::Relaxed) + 1;
            search_loading.set(true);
            search_error.set(String::new());
            let q = q.clone();
            spawn_local(async move {
                let res = api::search_apple_podcasts(&q, SEARCH_LIMIT).await;
                // Drop the response if a newer search has already started.
                if SEARCH_REQ_ID.load(Ordering::Relaxed) != req_id {
                    return;
                }
                match res {
                    Ok(env) => {
                        let filtered: Vec<_> = env
                            .results
                            .into_iter()
                            .filter(|r| r.feed_url.as_deref().is_some_and(|u| !u.is_empty()))
                            .collect();
                        search_results.set(filtered);
                        search_has_searched.set(true);
                        search_loading.set(false);
                    }
                    Err(_) => {
                        search_results.set(Vec::new());
                        search_has_searched.set(true);
                        search_loading.set(false);
                        search_error.set(
                            "Apple podcast search is unavailable right now.".to_string(),
                        );
                    }
                }
            });
        });
        SEARCH_DEBOUNCE.with(|cell| *cell.borrow_mut() = Some(timeout));
    };

    let on_search_input = move |ev: leptos::ev::Event| {
        let target = ev.target().unwrap();
        let input: HtmlInputElement = target.unchecked_into();
        trigger_search(input.value().trim().to_string());
    };

    let feed_url = RwSignal::new(String::new());
    let feed_busy = RwSignal::new(false);
    let feed_error = RwSignal::new(String::new());

    let do_subscribe_feed = {
        let on_added = on_added_url.clone();
        move || {
            let url = feed_url.get_untracked().trim().to_string();
            if url.is_empty() || feed_busy.get_untracked() {
                return;
            }
            feed_busy.set(true);
            feed_error.set(String::new());
            let on_added = on_added.clone();
            spawn_local(async move {
                match api::subscribe_podcast(&url).await {
                    Ok(_) => {
                        feed_url.set(String::new());
                        on_added();
                    }
                    Err(e) => feed_error.set(e.0),
                }
                feed_busy.set(false);
            });
        }
    };

    let do_subscribe_feed_btn = do_subscribe_feed.clone();
    let do_subscribe_feed_kbd = do_subscribe_feed.clone();

    view! {
        <div class="add-panels">
            // ---- Apple search panel --------------------------------------
            <section class="dashboard-panel">
                <div class="dashboard-panel-header">
                    <div>
                        <div class="dashboard-panel-title">"Search Apple Podcasts"</div>
                        <div class="dashboard-panel-copy">
                            "Start typing a show or creator name. Selecting a result subscribes immediately."
                        </div>
                    </div>
                </div>
                <div class="search-bar">
                    <input class="form-input"
                           type="search"
                           placeholder="Search by title or creator"
                           autocomplete="off"
                           spellcheck="false"
                           on:input=on_search_input />
                </div>
                <div class="search-error">
                    <Show when=move || !search_error.get().is_empty()>
                        <div class="error-msg">{move || search_error.get()}</div>
                    </Show>
                </div>
                <SearchResults
                    query=search_query.into()
                    results=search_results.into()
                    loading=search_loading.into()
                    has_searched=search_has_searched.into()
                    subscribing_feed_url=subscribing_feed_url
                    on_subscribed=on_added_search.clone() />
            </section>

            // ---- Raw RSS URL panel ---------------------------------------
            <section class="dashboard-panel">
                <div class="dashboard-panel-header">
                    <div>
                        <div class="dashboard-panel-title">"Add By RSS URL"</div>
                        <div class="dashboard-panel-copy">
                            "Paste a feed URL if the podcast is not in Apple search."
                        </div>
                    </div>
                </div>
                <div class="add-feed-bar">
                    <input class="form-input"
                           type="url"
                           placeholder="https://example.com/feed.xml"
                           prop:value=move || feed_url.get()
                           on:input=move |ev| {
                               let t = ev.target().unwrap();
                               let i: HtmlInputElement = t.unchecked_into();
                               feed_url.set(i.value());
                           }
                           on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                               if ev.key() == "Enter" {
                                   ev.prevent_default();
                                   do_subscribe_feed_kbd();
                               }
                           } />
                    <button class="btn btn-primary"
                            disabled=move || feed_busy.get()
                            on:click=move |_| do_subscribe_feed_btn()>
                        {move || if feed_busy.get() { "..." } else { "Add" }}
                    </button>
                </div>
                <Show when=move || !feed_error.get().is_empty()>
                    <div class="error-msg">{move || feed_error.get()}</div>
                </Show>
            </section>
        </div>
    }
}

#[component]
fn SearchResults(
    query: Signal<String>,
    results: Signal<Vec<api::AppleSearchResult>>,
    loading: Signal<bool>,
    has_searched: Signal<bool>,
    subscribing_feed_url: RwSignal<String>,
    #[prop()] on_subscribed: impl Fn() + 'static + Clone + Send + Sync,
) -> impl IntoView {
    view! {
        <div>
            {move || {
                let q = query.get();
                if q.chars().count() < SEARCH_MIN_CHARS {
                    return if !q.is_empty() {
                        let remaining = SEARCH_MIN_CHARS - q.chars().count();
                        let s = if remaining == 1 { "" } else { "s" };
                        view! {
                            <div class="search-empty">
                                {format!("Type {} more character{} to search Apple Podcasts.", remaining, s)}
                            </div>
                        }.into_any()
                    } else {
                        view! { <div></div> }.into_any()
                    };
                }
                if loading.get() {
                    return view! {
                        <div class="loading-state">"SEARCHING APPLE PODCASTS..."</div>
                    }.into_any();
                }
                let res = results.get();
                if res.is_empty() {
                    let msg = if has_searched.get() {
                        "No podcast matches found for this search."
                    } else {
                        "Search results will appear here."
                    };
                    return view! { <div class="search-empty">{msg}</div> }.into_any();
                }
                let on_subscribed = on_subscribed.clone();
                view! {
                    <div class="search-results">
                        <For
                            each={move || results.get()}
                            key={|r: &api::AppleSearchResult| r.feed_url.clone().unwrap_or_default()}
                            children={move |r: api::AppleSearchResult| {
                                let feed_url = r.feed_url.clone().unwrap_or_default();
                                let feed_url_for_click = feed_url.clone();
                                let track_name = r.track_name.clone()
                                    .or_else(|| r.collection_name.clone())
                                    .unwrap_or_else(|| "Untitled Podcast".to_string());
                                let artist = r.artist_name.clone()
                                    .unwrap_or_else(|| "Unknown creator".to_string());
                                let artwork = r.artwork_url_600.clone()
                                    .or_else(|| r.artwork_url_100.clone())
                                    .unwrap_or_default();
                                let genre = r.primary_genre_name.clone().unwrap_or_default();
                                let release = r.release_date.clone()
                                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                                    .map(|d| format_date(&d.with_timezone(&chrono::Utc)))
                                    .unwrap_or_default();
                                let on_subscribed = on_subscribed.clone();
                                // Clone the feed URL once per closure-capture site below; closures
                                // own their captures and we use them in many overlapping spots.
                                let fu_status = feed_url.clone();
                                let fu_status_2 = feed_url.clone();
                                let fu_status_3 = feed_url.clone();
                                let fu_status_4 = feed_url.clone();
                                let fu_busy = feed_url.clone();
                                let fu_busy_2 = feed_url.clone();
                                let podcasts_signal = use_app_state().podcasts;

                                view! {
                                    <div class="search-result-card">
                                        <ImageWithPlaceholder src=Signal::derive(move || artwork.clone()) size=72 />
                                        <div class="search-result-info">
                                            <div class="search-result-title">{track_name}</div>
                                            <div class="search-result-artist">{artist}</div>
                                            <div class="search-result-meta">
                                                <Show when={
                                                    let g = genre.clone();
                                                    move || !g.is_empty()
                                                }>
                                                    <span>{genre.clone()}</span>
                                                </Show>
                                                <Show when={
                                                    let r = release.clone();
                                                    move || !r.is_empty()
                                                }>
                                                    <span>{release.clone()}</span>
                                                </Show>
                                            </div>
                                            <div class="search-result-actions">
                                                <span class="search-result-status">
                                                    {move || if podcasts_signal.get().iter().any(|p| p.feed_url == fu_status) {
                                                        "In Library"
                                                    } else {
                                                        "Feed Ready"
                                                    }}
                                                </span>
                                                <button
                                                    class={move || if podcasts_signal.get().iter().any(|p| p.feed_url == fu_status_2) {
                                                        "btn btn-secondary search-subscribe-btn"
                                                    } else {
                                                        "btn btn-primary search-subscribe-btn"
                                                    }}
                                                    disabled={move || podcasts_signal.get().iter().any(|p| p.feed_url == fu_status_3) || subscribing_feed_url.get() == fu_busy}
                                                    on:click={
                                                        let on_subscribed = on_subscribed.clone();
                                                        let feed_url_for_click = feed_url_for_click.clone();
                                                        move |_| {
                                                            if subscribing_feed_url.get_untracked() == feed_url_for_click {
                                                                return;
                                                            }
                                                            subscribing_feed_url.set(feed_url_for_click.clone());
                                                            let on_subscribed = on_subscribed.clone();
                                                            let url = feed_url_for_click.clone();
                                                            spawn_local(async move {
                                                                if api::subscribe_podcast(&url).await.is_ok() {
                                                                    on_subscribed();
                                                                }
                                                                subscribing_feed_url.set(String::new());
                                                            });
                                                        }
                                                    }>
                                                    {move || if subscribing_feed_url.get() == fu_busy_2 {
                                                        "...".to_string()
                                                    } else if podcasts_signal.get().iter().any(|p| p.feed_url == fu_status_4) {
                                                        "Added".to_string()
                                                    } else {
                                                        "Subscribe".to_string()
                                                    }}
                                                </button>
                                            </div>
                                        </div>
                                    </div>
                                }
                            }}
                        />
                    </div>
                }.into_any()
            }}
        </div>
    }
}

// Bring `format_date_opt` into scope (used elsewhere). Keeps the imports
// section tidy without unused warnings.
#[allow(dead_code)]
fn _import_check() -> String {
    format_date_opt(&None)
}
