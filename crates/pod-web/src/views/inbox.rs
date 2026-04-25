// ==============================================================================
// Inbox view
// ==============================================================================
//
// Cross-podcast list of unfinished episodes. Sorted newest-first by the
// server. Each row supports play/pause, "done", and opening the episode
// detail modal.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use pod_model::{EpisodeWithProgress, InboxEpisode};

use crate::api;
use crate::player;
use crate::state::{use_app_state, PodcastCtx};
use crate::util::{format_date, format_duration, ICON_PAUSE, ICON_PLAY};
use crate::views::episode_modal::{open_episode_modal, EpisodeModalCtx};

const PER_PAGE: u32 = 30;

#[component]
pub fn InboxView() -> impl IntoView {
    let st = use_app_state();
    st.breadcrumb.set(String::new());

    let items = RwSignal::new(Vec::<InboxEpisode>::new());
    let next_token = RwSignal::new(Option::<String>::None);
    let load_error = RwSignal::new(String::new());
    let loading = RwSignal::new(true);
    let busy_more = RwSignal::new(false);

    let load_more = move || {
        let tok = next_token.get_untracked();
        spawn_local(async move {
            match api::list_inbox(tok.as_deref(), PER_PAGE).await {
                Ok(page) => {
                    items.update(|v| v.extend(page.items));
                    next_token.set(page.next_page_token);
                }
                Err(e) => load_error.set(e.0),
            }
            loading.set(false);
            busy_more.set(false);
        });
    };

    Effect::new(move |_| {
        items.set(Vec::new());
        next_token.set(None);
        loading.set(true);
        load_more();
    });

    let load_more_click = move |_| {
        if next_token.get_untracked().is_none() || busy_more.get_untracked() {
            return;
        }
        busy_more.set(true);
        load_more();
    };

    view! {
        <div class="dashboard-tabs">
            <A href="/" attr:class="dashboard-tab">"Subscriptions"</A>
            <button class="dashboard-tab active">"Inbox"</button>
        </div>
        <div class="section-label">"New Episodes"</div>
        {move || {
            if loading.get() && items.get().is_empty() {
                view! { <div class="loading-state">"LOADING..."</div> }.into_any()
            } else if !load_error.get().is_empty() && items.get().is_empty() {
                view! { <div class="error-msg">{load_error.get()}</div> }.into_any()
            } else if items.get().is_empty() {
                view! {
                    <div class="empty-state">
                        "No new episodes. You're all caught up!"
                    </div>
                }.into_any()
            } else {
                view! { <InboxList items=items /> }.into_any()
            }
        }}
        <Show when=move || next_token.get().is_some()>
            <button class="btn btn-secondary load-more-btn"
                    disabled=move || busy_more.get()
                    on:click=load_more_click>
                {move || if busy_more.get() { "..." } else { "Load More" }}
            </button>
        </Show>
    }
}

#[component]
fn InboxList(items: RwSignal<Vec<InboxEpisode>>) -> impl IntoView {
    view! {
        <ul class="episode-list">
            <For
                each={move || items.get()}
                key={|item: &InboxEpisode| item.episode.id.clone()}
                children={move |item: InboxEpisode| view! {
                    <InboxRow item=item items=items />
                }}
            />
        </ul>
    }
}

#[component]
fn InboxRow(item: InboxEpisode, items: RwSignal<Vec<InboxEpisode>>) -> impl IntoView {
    let st = use_app_state();
    let nav = use_navigate();

    let ep = item.episode.clone();
    let podcast_id = ep.podcast_id.clone();
    let podcast_title = item.podcast_title.clone();
    let podcast_image = item.podcast_image_link.clone();

    let pod_ctx = PodcastCtx::from(&item);

    let ep_id = ep.id.clone();
    let ep_id_for_active = ep_id.clone();
    let ep_id_for_play_a = ep_id.clone();
    let ep_id_for_play_b = ep_id.clone();
    let ep_id_for_play_click = ep_id.clone();
    let ep_id_for_detail = ep_id.clone();

    let _ = ep_id_for_play_b;
    let is_active = Signal::derive(move || {
        st.player
            .get()
            .map(|p| p.item.episode.id == ep_id_for_active)
            .unwrap_or(false)
    });
    let is_playing = Signal::derive(move || {
        st.player
            .get()
            .map(|p| p.item.episode.id == ep_id_for_play_a && p.playing)
            .unwrap_or(false)
    });

    let progress = item.progress.unwrap_or(0);
    let duration = ep.audio_duration as i64;
    let pct = if duration > 0 {
        (progress as i64 * 100 / duration).clamp(0, 100)
    } else {
        0
    };

    let title = ep.title.clone();
    let pub_date = format_date(&ep.publication_date);
    let dur_label = format_duration(ep.audio_duration as i64, true);

    let pod_ctx_play = pod_ctx.clone();

    let on_play_click = move |_| {
        let snapshot = items
            .get_untracked()
            .iter()
            .find(|i| i.episode.id == ep_id_for_play_click)
            .cloned();
        let Some(item) = snapshot else { return };

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
        let wrapped = EpisodeWithProgress {
            episode: item.episode.clone(),
            progress: item.progress,
            done: item.done,
        };
        player::play_episode(wrapped, pod_ctx_play.clone());
    };

    let pod_ctx_detail = pod_ctx.clone();
    let items_detail = items;
    let on_title_click = move |_| {
        let snapshot = items_detail
            .get_untracked()
            .iter()
            .find(|i| i.episode.id == ep_id_for_detail)
            .cloned();
        let Some(item) = snapshot else { return };
        let wrapped = EpisodeWithProgress {
            episode: item.episode.clone(),
            progress: item.progress,
            done: item.done,
        };
        open_episode_modal(EpisodeModalCtx {
            item: wrapped,
            podcast: Some(pod_ctx_detail.clone()),
        });
    };

    let nav_pod = {
        let nav = nav.clone();
        let pid = podcast_id.clone();
        move |_| nav(&format!("/podcast/{}", pid), Default::default())
    };

    let ep_id_done = item.episode.id.clone();
    let on_done_click = move |_| {
        let items_now = items.get_untracked();
        let pos = items_now.iter().position(|i| i.episode.id == ep_id_done);
        let Some(pos) = pos else { return };
        let target = items_now[pos].clone();
        let new_done = !crate::util::is_episode_done(&EpisodeWithProgress {
            episode: target.episode.clone(),
            progress: target.progress,
            done: target.done,
        });
        let progress = target.progress.unwrap_or(0);
        let ep_id = target.episode.id.clone();
        spawn_local(async move {
            if api::report_progress(&ep_id, progress, new_done).await.is_ok() && new_done {
                items.update(|v| {
                    if let Some(p) = v.iter().position(|i| i.episode.id == ep_id) {
                        v.remove(p);
                    }
                });
            }
        });
    };

    let _ = podcast_image;

    view! {
        <li class="episode-item"
            class:active=is_active>
            <button class="episode-play-btn"
                    class:playing=is_playing
                    on:click=on_play_click
                    inner_html=move || if is_playing.get() { ICON_PAUSE } else { ICON_PLAY }>
            </button>
            <div class="episode-info">
                <div class="inbox-episode-podcast" on:click=nav_pod>{podcast_title}</div>
                <div class="episode-title" on:click=on_title_click>{title}</div>
                <Show when={move || pct > 0}>
                    <div class="episode-progress-bar">
                        <div class="episode-progress-fill"
                             style:width=format!("{}%", pct)>
                        </div>
                    </div>
                </Show>
            </div>
            <div class="episode-right">
                <div class="episode-meta">
                    <span>{pub_date}</span>
                    <span>{dur_label}</span>
                </div>
                <button class="episode-done-btn"
                        title="Mark as done"
                        on:click=on_done_click>
                    "Done"
                </button>
            </div>
        </li>
    }
}
