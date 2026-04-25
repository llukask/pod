// ==============================================================================
// App shell + persistent player bar
// ==============================================================================
//
// Wraps the routed views with the top header (logo / breadcrumb / username /
// logout) and the bottom player bar. Player state lives in `AppState` and is
// driven by `crate::player`.

use std::cell::RefCell;

use gloo_events::EventListener;
use gloo_timers::callback::Interval;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use wasm_bindgen::JsCast;

use crate::api;
use crate::player;
use crate::state::use_app_state;
use crate::util::{format_duration, ICON_PAUSE, ICON_PLAY};

// EventListener and Interval contain `!Send` closures, so they can't live
// inside leptos's default (sync) reactive storage. Since these resources
// are app-lifetime singletons (keyboard shortcuts, beforeunload, the player
// poll), we park them in thread-locals and never drop them. On wasm32 the
// page == the thread, so this is equivalent to leaking with `.forget()`.
thread_local! {
    static SHELL_LISTENERS: RefCell<Vec<EventListener>> = const { RefCell::new(Vec::new()) };
    static PLAYER_TICK: RefCell<Option<Interval>> = const { RefCell::new(None) };
}

#[component]
pub fn AppShell(children: Children) -> impl IntoView {
    let st = use_app_state();
    let nav = use_navigate();

    // ----- Keyboard shortcuts (space / left / right) -------------------------
    //
    // Attached at the window level once and parked in a thread-local so the
    // listener outlives any individual shell mount/unmount cycle. The
    // app-shell only ever mounts once per page in practice (it's gated
    // behind the auth check), so re-running this is harmless.
    SHELL_LISTENERS.with(|cell| {
        if !cell.borrow().is_empty() {
            return;
        }
        let window = web_sys::window().expect("window in browser context");
        let keydown = EventListener::new(&window, "keydown", move |ev| {
            let kev: web_sys::KeyboardEvent = ev.clone().dyn_into().unwrap();
            // Don't steal keys typed into form fields.
            if let Some(target) = kev.target() {
                if let Ok(el) = target.dyn_into::<web_sys::Element>() {
                    let tag = el.tag_name();
                    if tag == "INPUT" || tag == "TEXTAREA" {
                        return;
                    }
                }
            }
            if use_app_state().player.get_untracked().is_none() {
                return;
            }
            match kev.code().as_str() {
                "Space" => {
                    kev.prevent_default();
                    player::toggle_play_pause();
                }
                "ArrowLeft" => {
                    kev.prevent_default();
                    player::skip(-15.0);
                }
                "ArrowRight" => {
                    kev.prevent_default();
                    player::skip(30.0);
                }
                _ => {}
            }
        });
        let beforeunload =
            EventListener::new(&window, "beforeunload", |_| player::save_progress());
        cell.borrow_mut().push(keydown);
        cell.borrow_mut().push(beforeunload);
    });

    let logout = {
        let nav = nav.clone();
        move |_| {
            // Fire-and-forget logout call.
            spawn_local(async move {
                let _ = api::logout().await;
            });
            use_app_state().force_logout();
            nav("/", Default::default());
        }
    };

    let go_home = {
        let nav = nav.clone();
        move |_| nav("/", Default::default())
    };

    view! {
        <div class="app-layout">
            <header class="app-header">
                <div class="app-logo-group">
                    <div class="app-logo" on:click=go_home>"POD"</div>
                    <span class="header-breadcrumb">{move || st.breadcrumb.get()}</span>
                </div>
                <div class="app-header-right">
                    <span class="app-username">{move || st.username.get()}</span>
                    <button class="logout-btn" on:click=logout>"Logout"</button>
                </div>
            </header>
            <main class="app-content"
                  class:has-player=move || st.player.get().is_some()>
                {children()}
            </main>
            <PlayerBar />
        </div>
    }
}

// ------------------------------------------------------------------------------
// Player bar
// ------------------------------------------------------------------------------

#[component]
fn PlayerBar() -> impl IntoView {
    let st = use_app_state();
    let nav = use_navigate();

    // The player bar polls the audio element 4x/second to update its seek
    // bar and time readout. The interval lives for page lifetime in a
    // thread-local; like the shell-level listeners above, the player bar
    // only mounts once per session in practice.
    let now_secs = RwSignal::new((0.0_f64, 0.0_f64));
    PLAYER_TICK.with(|cell| {
        if cell.borrow().is_some() {
            return;
        }
        let interval = Interval::new(250, move || {
            if use_app_state().player.get_untracked().is_some() {
                now_secs.set((player::current_time(), player::duration()));
            }
        });
        *cell.borrow_mut() = Some(interval);
    });

    let seek_pct = move || {
        let (t, d) = now_secs.get();
        if d.is_finite() && d > 0.0 {
            (t / d) * 100.0
        } else {
            0.0
        }
    };

    let time_label = move || {
        let (t, d) = now_secs.get();
        let dur = if d.is_finite() && d > 0.0 {
            d as i64
        } else {
            st.player
                .get()
                .map(|p| p.item.episode.audio_duration as i64)
                .unwrap_or(0)
        };
        format!(
            "{} / {}",
            format_duration(t as i64, false),
            format_duration(dur, false)
        )
    };

    let on_seek = move |ev: leptos::ev::MouseEvent| {
        if use_app_state().player.get_untracked().is_none() {
            return;
        }
        if let Some(target) = ev.current_target() {
            if let Ok(el) = target.dyn_into::<web_sys::Element>() {
                let rect = el.get_bounding_client_rect();
                let ratio =
                    ((ev.client_x() as f64 - rect.left()) / rect.width()).clamp(0.0, 1.0);
                player::seek_ratio(ratio);
            }
        }
    };

    let go_to_podcast = {
        let nav = nav.clone();
        move |_| {
            if let Some(p) = use_app_state().player.get_untracked() {
                nav(&format!("/podcast/{}", p.podcast.id), Default::default());
            }
        }
    };

    view! {
        <div
            class="player-bar"
            class:visible=move || st.player.get().is_some()>
            <div class="player-seek" on:click=on_seek>
                <div class="player-seek-fill"
                     style:width=move || format!("{:.2}%", seek_pct())>
                </div>
            </div>
            <div class="player-controls">
                <div class="player-detail-link">
                    <img class="player-thumb"
                         src=move || st.player.get().map(|p| {
                             p.item.episode.thumbnail_url.unwrap_or_else(|| p.podcast.image_link.clone())
                         }).unwrap_or_default()
                         alt="Episode artwork" />
                    <div class="player-info">
                        <div class="player-ep-title">
                            {move || st.player.get().map(|p| p.item.episode.title).unwrap_or_default()}
                        </div>
                        <div class="player-pod-name" on:click=go_to_podcast>
                            {move || st.player.get().map(|p| p.podcast.title).unwrap_or_default()}
                        </div>
                    </div>
                </div>
                <div class="player-transport">
                    <button class="player-skip" on:click=|_| player::skip(-15.0)>"-15s"</button>
                    <button class="player-main-btn"
                            on:click=|_| player::toggle_play_pause()
                            inner_html=move || {
                                if st.player.get().map(|p| p.playing).unwrap_or(false) {
                                    ICON_PAUSE
                                } else {
                                    ICON_PLAY
                                }
                            }>
                    </button>
                    <button class="player-skip" on:click=|_| player::skip(30.0)>"+30s"</button>
                </div>
                <div class="player-time">{time_label}</div>
                <div class="player-shortcuts-hint"
                     title="Space: play/pause&#10;←: -15s&#10;→: +30s">
                    "?"
                </div>
            </div>
        </div>
    }
}
