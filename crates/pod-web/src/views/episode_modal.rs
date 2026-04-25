// ==============================================================================
// Episode detail modal
// ==============================================================================
//
// The modal is mounted dynamically onto a fresh host element appended to
// `<body>` (rather than living inside the routed view tree) so it can sit
// above all routes. The host is removed when the user dismisses the modal.
//
// Show notes are sanitized via DOMPurify (loaded from CDN in `index.html`).

use std::cell::RefCell;
use std::sync::Arc;

use gloo_events::EventListener;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use pod_model::EpisodeWithProgress;
use wasm_bindgen::JsCast;

use crate::api;
use crate::player;
use crate::state::{use_app_state, PodcastCtx};
use crate::util::{
    format_date, format_duration, is_episode_done, sanitize_html, ICON_PAUSE, ICON_PLAY,
};

// Per-page state for the currently-open modal. Only one modal is ever open
// at a time, so a single set of thread-local slots is enough. Living in
// thread-locals (rather than being captured by the close closure) lets the
// close callback be `Send + Sync` without dragging `web_sys::Element` —
// which is `!Send` — across thread bounds in the type system.
thread_local! {
    static MODAL_KEYDOWN: RefCell<Option<EventListener>> = const { RefCell::new(None) };
    static MODAL_HOST: RefCell<Option<web_sys::Element>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub struct EpisodeModalCtx {
    pub item: EpisodeWithProgress,
    pub podcast: Option<PodcastCtx>,
}

/// Mount the episode detail modal under `<body>`. Idempotent — calling it
/// twice replaces the previous modal.
pub fn open_episode_modal(ctx: EpisodeModalCtx) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(body) = document.body() else {
        return;
    };

    // Tear down any previous modal first.
    close_open_modal();

    let host = match document.create_element("div") {
        Ok(el) => el,
        Err(_) => return,
    };
    let _ = body.append_child(&host);

    MODAL_HOST.with(|cell| *cell.borrow_mut() = Some(host.clone()));

    // The close callback pulls the host out of the thread-local on click,
    // which keeps `web_sys::Element` (a `!Send` type) out of its captured
    // environment. That lets `Arc<dyn Fn() + Send + Sync>` hold this
    // closure, which is required by leptos's children bound on the Show
    // component used below.
    let close: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
        MODAL_KEYDOWN.with(|cell| cell.borrow_mut().take());
        MODAL_HOST.with(|cell| {
            if let Some(host) = cell.borrow_mut().take() {
                if let Some(parent) = host.parent_node() {
                    let _ = parent.remove_child(&host);
                }
            }
        });
    });

    // Esc-to-close. We keep the listener in a thread-local rather than
    // tying its lifetime to the leptos owner so the leptos owner doesn't
    // need to hold a `!Send` value.
    let close_for_key = close.clone();
    let keydown = EventListener::new(&window, "keydown", move |ev| {
        let kev: web_sys::KeyboardEvent = ev.clone().unchecked_into();
        if kev.key() == "Escape" {
            close_for_key();
        } else if kev.code() == "Space" {
            // Don't let the global player shortcut fire while the modal
            // is open.
            kev.stop_propagation();
        }
    });
    MODAL_KEYDOWN.with(|cell| *cell.borrow_mut() = Some(keydown));

    // Mount the Leptos modal into the host element. `mount_to` returns a
    // handle whose Drop unmounts the tree; `forget()` keeps it alive until
    // the host is removed by the close handler.
    let close_for_view = close.clone();
    let handle = leptos::mount::mount_to(
        host.clone().unchecked_into::<web_sys::HtmlElement>(),
        move || view! { <EpisodeModal ctx=ctx.clone() close=close_for_view.clone() /> },
    );
    handle.forget();
}

fn close_open_modal() {
    MODAL_KEYDOWN.with(|cell| cell.borrow_mut().take());
    // We don't track the previous host here — the close fn captured by the
    // mounted view does that. This is a no-op when no modal is open.
}

#[component]
fn EpisodeModal(ctx: EpisodeModalCtx, close: Arc<dyn Fn() + Send + Sync>) -> impl IntoView {
    let nav = use_navigate();
    let st = use_app_state();

    let ep = ctx.item.episode.clone();
    let item_signal = RwSignal::new(ctx.item.clone());

    // Pick `content_encoded` (full show notes) over `summary` and decide
    // whether to treat the body as HTML based on the *_type fields.
    let body_html = {
        let raw_html_pref = !ep.content_encoded.is_empty();
        let raw = if raw_html_pref {
            ep.content_encoded.clone()
        } else {
            ep.summary.clone()
        };
        let is_html = if raw_html_pref {
            ep.content_encoded_type.contains("html")
        } else {
            ep.summary_type.contains("html")
        };
        if raw.is_empty() {
            "<em>No description available.</em>".to_string()
        } else if is_html {
            sanitize_html(&raw)
        } else {
            html_escape(&raw).replace('\n', "<br>")
        }
    };

    let pod_ctx_link = ctx.podcast.clone();
    let pod_ctx_play = ctx.podcast.clone();

    let close_btn = close.clone();
    let close_backdrop = close.clone();
    let close_link = close.clone();

    let ep_id = ep.id.clone();
    let ep_id_play = ep_id.clone();

    let is_playing_signal = move || {
        st.player
            .get()
            .map(|p| p.item.episode.id == ep_id && p.playing)
            .unwrap_or(false)
    };

    let on_play_click = move |_| {
        let st = use_app_state();
        if let Some(p) = st.player.get_untracked() {
            if p.item.episode.id == ep_id_play {
                if p.playing {
                    player::pause_audio();
                } else {
                    player::resume_audio();
                }
                return;
            }
        }
        let Some(pod) = pod_ctx_play.clone() else {
            return;
        };
        player::play_episode(item_signal.get_untracked(), pod);
    };

    let on_done_click = move |_| {
        let item = item_signal.get_untracked();
        let progress = item.progress.unwrap_or(0);
        let new_done = !is_episode_done(&item);
        let ep_id = item.episode.id.clone();
        spawn_local(async move {
            if api::report_progress(&ep_id, progress, new_done).await.is_ok() {
                item_signal.update(|i| {
                    i.done = new_done;
                });
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

    let on_pod_link_click = {
        let nav = nav.clone();
        move |_| {
            if let Some(pod) = pod_ctx_link.clone() {
                close_link();
                nav(&format!("/podcast/{}", pod.id), Default::default());
            }
        }
    };

    let pod_title = ctx.podcast.as_ref().map(|p| p.title.clone());
    let has_pod = pod_title.is_some();

    view! {
        <div
            class="episode-modal-backdrop"
            on:click=move |ev: leptos::ev::MouseEvent| {
                if let Some(target) = ev.target() {
                    if let Some(current) = ev.current_target() {
                        if target.eq(&current) {
                            close_backdrop();
                        }
                    }
                }
            }>
            <div class="episode-modal">
                <div class="episode-modal-header">
                    <div>
                        <h3>{ep.title.clone()}</h3>
                        <div class="episode-meta">
                            <span>{format_date(&ep.publication_date)}</span>
                            <span>{format_duration(ep.audio_duration as i64, true)}</span>
                            <Show when=move || has_pod>
                                <span class="modal-podcast-link"
                                      style="cursor:pointer"
                                      on:click=on_pod_link_click.clone()>
                                    {pod_title.clone().unwrap_or_default()}
                                </span>
                            </Show>
                        </div>
                        <div style="display:flex;gap:8px;align-items:center;margin-top:8px">
                            <button class="episode-modal-play"
                                    on:click=on_play_click
                                    inner_html=move || {
                                        let icon = if is_playing_signal() { ICON_PAUSE } else { ICON_PLAY };
                                        let label = if is_playing_signal() { "Pause" } else { "Play" };
                                        format!("{} {}", icon, label)
                                    }>
                            </button>
                            <button
                                class="episode-done-btn"
                                class:is-done=move || is_episode_done(&item_signal.get())
                                on:click=on_done_click>
                                {move || if is_episode_done(&item_signal.get()) {
                                    "✓ Done".to_string()
                                } else {
                                    "Mark Done".to_string()
                                }}
                            </button>
                        </div>
                    </div>
                    <button class="episode-modal-close"
                            on:click=move |_| close_btn()>
                        "×"
                    </button>
                </div>
                <div class="episode-modal-body" inner_html=body_html.clone()></div>
            </div>
        </div>
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
