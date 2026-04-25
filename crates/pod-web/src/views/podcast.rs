// ==============================================================================
// Podcast detail view
// ==============================================================================
//
// Shows the podcast header (artwork + title + description with show-more
// toggle) and the paged episode list. Each row hooks into the global player
// via `crate::player::play_episode`.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use pod_model::{EpisodeWithProgress, Podcast};

use crate::api;
use crate::player;
use crate::state::{use_app_state, PodcastCtx};
use crate::util::{format_date, format_duration, is_episode_done, strip_html, ICON_PAUSE, ICON_PLAY};
use crate::views::episode_modal::{open_episode_modal, EpisodeModalCtx};
use crate::views::shared::ImageWithPlaceholder;

const PER_PAGE: u32 = 30;

#[component]
pub fn PodcastView() -> impl IntoView {
    let params = use_params_map();
    let podcast_id =
        Signal::derive(move || params.read().get("id").map(String::from).unwrap_or_default());

    let podcast = RwSignal::new(Option::<Podcast>::None);
    let episodes = RwSignal::new(Vec::<EpisodeWithProgress>::new());
    let next_token = RwSignal::new(Option::<String>::None);
    let load_error = RwSignal::new(String::new());
    let loading = RwSignal::new(true);
    let busy_more = RwSignal::new(false);

    let load_more_episodes = move |id: String| {
        let tok = next_token.get_untracked();
        spawn_local(async move {
            match api::list_episodes(&id, tok.as_deref(), PER_PAGE).await {
                Ok(page) => {
                    episodes.update(|v| v.extend(page.items));
                    next_token.set(page.next_page_token);
                }
                Err(e) => load_error.set(e.0),
            }
            busy_more.set(false);
        });
    };

    Effect::new(move |_| {
        let id = podcast_id.get();
        if id.is_empty() {
            return;
        }
        // Reset state when the podcast id changes (e.g. navigating from one
        // podcast detail directly to another via the player bar).
        podcast.set(None);
        episodes.set(Vec::new());
        next_token.set(None);
        load_error.set(String::new());
        loading.set(true);

        let id_for_meta = id.clone();
        spawn_local(async move {
            match api::get_podcast(&id_for_meta).await {
                Ok(p) => {
                    use_app_state().breadcrumb.set(p.title.clone());
                    podcast.set(Some(p));
                }
                Err(e) => load_error.set(e.0),
            }
            loading.set(false);
        });
        load_more_episodes(id);
    });

    let on_load_more = move |_| {
        let id = podcast_id.get_untracked();
        if id.is_empty() || next_token.get_untracked().is_none() || busy_more.get_untracked() {
            return;
        }
        busy_more.set(true);
        load_more_episodes(id);
    };

    view! {
        <A href="/" attr:class="back-btn">"← Back"</A>
        {move || {
            if loading.get() && podcast.get().is_none() {
                view! { <div class="loading-state">"LOADING..."</div> }.into_any()
            } else if !load_error.get().is_empty() {
                view! { <div class="error-msg">{load_error.get()}</div> }.into_any()
            } else if let Some(p) = podcast.get() {
                view! { <PodcastHeader podcast=p /> }.into_any()
            } else {
                view! { <div></div> }.into_any()
            }
        }}
        <div class="section-label">"Episodes"</div>
        <ul class="episode-list">
            <For
                each={move || episodes.get()}
                key={|e: &EpisodeWithProgress| e.episode.id.clone()}
                children={move |e: EpisodeWithProgress| view! {
                    <EpisodeRow item=e episodes=episodes podcast=podcast />
                }}
            />
        </ul>
        <Show when=move || next_token.get().is_some()>
            <button class="btn btn-secondary load-more-btn"
                    disabled=move || busy_more.get()
                    on:click=on_load_more>
                {move || if busy_more.get() { "..." } else { "Load More" }}
            </button>
        </Show>
    }
}

#[component]
fn PodcastHeader(podcast: Podcast) -> impl IntoView {
    let title = podcast.title.clone();
    let img = podcast.image_link.clone();
    let desc = strip_html(&podcast.description);
    let expanded = RwSignal::new(false);

    let desc_node: NodeRef<leptos::html::Div> = NodeRef::new();
    let show_more_visible = RwSignal::new(false);

    Effect::new(move |_| {
        // After the description renders, decide whether the "show more"
        // affordance should appear by comparing scrollHeight/clientHeight.
        if let Some(el) = desc_node.get() {
            use wasm_bindgen::JsCast;
            let element: &web_sys::Element = el.as_ref();
            let html_el: web_sys::HtmlElement = element.clone().unchecked_into();
            if html_el.scroll_height() > html_el.client_height() {
                show_more_visible.set(true);
            }
        }
    });

    view! {
        <div class="podcast-header">
            <ImageWithPlaceholder src=Signal::derive(move || img.clone()) size=120 />
            <div class="podcast-header-info">
                <div class="podcast-title">{title}</div>
                <div class="podcast-description"
                     class:expanded=move || expanded.get()
                     node_ref=desc_node>
                    {desc.clone()}
                </div>
                <Show when=move || show_more_visible.get()>
                    <button class="show-more-btn"
                            on:click=move |_| expanded.update(|v| *v = !*v)>
                        {move || if expanded.get() { "Show less" } else { "Show more" }}
                    </button>
                </Show>
            </div>
        </div>
    }
}

#[component]
fn EpisodeRow(
    item: EpisodeWithProgress,
    episodes: RwSignal<Vec<EpisodeWithProgress>>,
    podcast: RwSignal<Option<Podcast>>,
) -> impl IntoView {
    let st = use_app_state();
    let ep_id = item.episode.id.clone();

    let ep_id_active = ep_id.clone();
    let ep_id_play = ep_id.clone();
    let ep_id_done = ep_id.clone();

    let title = item.episode.title.clone();
    let pub_date = format_date(&item.episode.publication_date);
    let dur_label = format_duration(item.episode.audio_duration as i64, true);

    // Wrap the per-row reactive helpers as `Signal`s. Plain closures aren't
    // `Copy`, but `Signal<T>` is — so we can hand the same `Signal` to
    // multiple attribute slots in the view! macro without cloning.
    let is_active = Signal::derive(move || {
        st.player
            .get()
            .map(|p| p.item.episode.id == ep_id_active)
            .unwrap_or(false)
    });
    let is_playing = Signal::derive(move || {
        st.player
            .get()
            .map(|p| p.item.episode.id == ep_id_play && p.playing)
            .unwrap_or(false)
    });

    let ep_id_for_pct = ep_id.clone();
    let pct = Signal::derive(move || {
        episodes
            .get()
            .iter()
            .find(|i| i.episode.id == ep_id_for_pct)
            .map(|i| {
                let dur = i.episode.audio_duration as i64;
                if dur <= 0 {
                    return 0;
                }
                let p = i.progress.unwrap_or(0) as i64;
                (p * 100 / dur).clamp(0, 100)
            })
            .unwrap_or(0)
    });

    let ep_id_pct = item.episode.id.clone();
    let is_done_now = Signal::derive(move || {
        episodes
            .get()
            .iter()
            .find(|i| i.episode.id == ep_id_pct)
            .map(is_episode_done)
            .unwrap_or(false)
    });

    let ep_id_play_click = item.episode.id.clone();
    let on_play = move |_| {
        let snap = episodes
            .get_untracked()
            .iter()
            .find(|i| i.episode.id == ep_id_play_click)
            .cloned();
        let Some(item) = snap else { return };
        let st = use_app_state();
        if let Some(p) = st.player.get_untracked() {
            if p.item.episode.id == item.episode.id {
                if p.playing {
                    player::pause_audio();
                } else {
                    player::resume_audio();
                }
                return;
            }
        }
        let pod_ctx = match podcast.get_untracked() {
            Some(p) => PodcastCtx::from(&p),
            None => return,
        };
        player::play_episode(item, pod_ctx);
    };

    let ep_id_for_modal = item.episode.id.clone();
    let on_title_click = move |_| {
        let snap = episodes
            .get_untracked()
            .iter()
            .find(|i| i.episode.id == ep_id_for_modal)
            .cloned();
        let Some(item) = snap else { return };
        let pod_ctx = podcast.get_untracked().as_ref().map(PodcastCtx::from);
        open_episode_modal(EpisodeModalCtx {
            item,
            podcast: pod_ctx,
        });
    };

    let on_done = move |_| {
        let items = episodes.get_untracked();
        let target = items.iter().find(|i| i.episode.id == ep_id_done).cloned();
        let Some(target) = target else { return };
        let new_done = !is_episode_done(&target);
        let progress = target.progress.unwrap_or(0);
        let ep_id = target.episode.id.clone();
        spawn_local(async move {
            if api::report_progress(&ep_id, progress, new_done).await.is_ok() {
                episodes.update(|v| {
                    if let Some(it) = v.iter_mut().find(|i| i.episode.id == ep_id) {
                        it.done = new_done;
                    }
                });
                // Keep the player's in-memory item in sync if this is the
                // currently-playing episode.
                use_app_state().player.update(|p| {
                    if let Some(p) = p.as_mut() {
                        if p.item.episode.id == ep_id {
                            p.item.done = new_done;
                            p.manual_done_off = !new_done;
                        }
                    }
                });
            }
        });
    };

    view! {
        <li class="episode-item"
            class:active=is_active>
            <button class="episode-play-btn"
                    class:playing=is_playing
                    on:click=on_play
                    inner_html=move || if is_playing.get() { ICON_PAUSE } else { ICON_PLAY }>
            </button>
            <div class="episode-info">
                <div class="episode-title" on:click=on_title_click>{title}</div>
                <Show when={move || !is_done_now.get() && pct.get() > 0}>
                    <div class="episode-progress-bar">
                        <div class="episode-progress-fill"
                             style:width=move || format!("{}%", pct.get())>
                        </div>
                    </div>
                </Show>
            </div>
            <div class="episode-right">
                <div class="episode-meta">
                    <span>{pub_date}</span>
                    <span>{dur_label}</span>
                </div>
                <button
                    class="episode-done-btn"
                    class:is-done=is_done_now
                    on:click=on_done>
                    {move || if is_done_now.get() { "✓ Done" } else { "Done" }}
                </button>
            </div>
        </li>
    }
}
