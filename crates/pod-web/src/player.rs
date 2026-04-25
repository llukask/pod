// ==============================================================================
// Audio playback + progress auto-save
// ==============================================================================
//
// Owns a single `<audio>` element kept around for the lifetime of the page,
// plus a 15-second interval that persists progress to the server. Mirrors
// the JS frontend's player module 1:1.

use std::cell::RefCell;
use std::sync::atomic::{AtomicI32, Ordering};

use gloo_events::EventListener;
use gloo_timers::callback::Interval;
use leptos::prelude::*;
use web_sys::HtmlAudioElement;

use crate::api;
use crate::state::{use_app_state, PlayerItem, PodcastCtx};

const PROGRESS_INTERVAL_MS: u32 = 15_000;
/// We treat the episode as auto-done when the remaining time drops below this.
const AUTO_DONE_THRESHOLD_SECS: f64 = 30.0;

thread_local! {
    /// Lazily-initialized singleton audio element.
    static AUDIO: RefCell<Option<HtmlAudioElement>> = const { RefCell::new(None) };
    /// Active progress-save interval (dropped to cancel).
    static PROGRESS_TIMER: RefCell<Option<Interval>> = const { RefCell::new(None) };
    /// Audio element event listeners (kept alive for the page lifetime).
    static AUDIO_LISTENERS: RefCell<Vec<EventListener>> = const { RefCell::new(Vec::new()) };
}

/// `lastSavedProgress` from the JS version — avoids re-reporting unchanged
/// progress every tick. -1 means "no save yet for the current episode".
static LAST_SAVED_PROGRESS: AtomicI32 = AtomicI32::new(-1);

/// Lazily build the page-wide `<audio>` element and wire its events to the
/// reactive state. Subsequent calls return the same element.
fn audio() -> HtmlAudioElement {
    AUDIO.with(|cell| {
        if let Some(a) = cell.borrow().as_ref() {
            return a.clone();
        }
        let a = HtmlAudioElement::new().expect("HTMLAudioElement::new should not fail");
        wire_audio_events(&a);
        *cell.borrow_mut() = Some(a.clone());
        a
    })
}

fn wire_audio_events(a: &HtmlAudioElement) {
    let target: web_sys::EventTarget = a.clone().into();

    let on_play = EventListener::new(&target, "play", move |_| {
        let st = use_app_state();
        st.player.update(|p| {
            if let Some(p) = p.as_mut() {
                p.playing = true;
            }
        });
    });

    let on_pause = EventListener::new(&target, "pause", move |_| {
        let st = use_app_state();
        st.player.update(|p| {
            if let Some(p) = p.as_mut() {
                p.playing = false;
            }
        });
    });

    let on_ended = EventListener::new(&target, "ended", move |_| {
        stop_progress_timer();
        save_progress();
        let st = use_app_state();
        st.player.update(|p| {
            if let Some(p) = p.as_mut() {
                p.playing = false;
            }
        });
    });

    AUDIO_LISTENERS.with(|cell| {
        let mut v = cell.borrow_mut();
        v.push(on_play);
        v.push(on_pause);
        v.push(on_ended);
    });
}

/// Begin playing `item` on the audio element, seeking to its saved progress
/// if any. Saves progress for the previous episode first.
pub fn play_episode(item: pod_model::EpisodeWithProgress, podcast: PodcastCtx) {
    let st = use_app_state();

    if let Some(prev) = st.player.get_untracked() {
        if prev.playing {
            save_progress();
        }
    }

    let audio = audio();
    audio.set_src(&item.episode.audio_url);

    let seek_target = item.progress.unwrap_or(0).max(0) as f64;
    if seek_target > 0.0 {
        // Wire a one-shot `loadeddata` listener to seek once metadata is
        // available; the JS version does the same dance.
        let target: web_sys::EventTarget = audio.clone().into();
        let listener_slot: std::rc::Rc<std::cell::RefCell<Option<EventListener>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let listener_slot_inner = listener_slot.clone();
        let audio_inner = audio.clone();
        let listener = EventListener::new(&target, "loadeddata", move |_| {
            audio_inner.set_current_time(seek_target);
            // Drop the listener after it fires.
            *listener_slot_inner.borrow_mut() = None;
        });
        *listener_slot.borrow_mut() = Some(listener);
    }

    let _ = audio.play();

    LAST_SAVED_PROGRESS.store(-1, Ordering::Relaxed);
    st.player.set(Some(PlayerItem {
        item,
        podcast,
        playing: true,
        manual_done_off: false,
    }));
    start_progress_timer();
}

pub fn pause_audio() {
    audio().pause().ok();
    save_progress();
    stop_progress_timer();
}

pub fn resume_audio() {
    let _ = audio().play();
    start_progress_timer();
}

pub fn toggle_play_pause() {
    let st = use_app_state();
    let Some(p) = st.player.get_untracked() else {
        return;
    };
    if p.playing {
        pause_audio();
    } else {
        resume_audio();
    }
}

pub fn skip(secs: f64) {
    let st = use_app_state();
    if st.player.get_untracked().is_none() {
        return;
    }
    let a = audio();
    let new = (a.current_time() + secs).max(0.0).min(a.duration());
    a.set_current_time(new);
}

/// Seek to a fractional position [0, 1] on the audio.
pub fn seek_ratio(ratio: f64) {
    let a = audio();
    let dur = a.duration();
    if dur.is_finite() && dur > 0.0 {
        a.set_current_time((ratio.clamp(0.0, 1.0)) * dur);
    }
}

/// Live current-time / duration (in seconds) for the player bar UI.
pub fn current_time() -> f64 {
    audio().current_time()
}
pub fn duration() -> f64 {
    audio().duration()
}

/// Hard stop — used by `force_logout` to release the audio resource.
pub fn stop_audio() {
    AUDIO.with(|cell| {
        if let Some(a) = cell.borrow().as_ref() {
            let _ = a.pause();
            a.set_src("");
        }
    });
    stop_progress_timer();
}

fn start_progress_timer() {
    stop_progress_timer();
    let interval = Interval::new(PROGRESS_INTERVAL_MS, save_progress);
    PROGRESS_TIMER.with(|cell| *cell.borrow_mut() = Some(interval));
}

fn stop_progress_timer() {
    PROGRESS_TIMER.with(|cell| cell.borrow_mut().take());
}

/// Persist current playback position. Best-effort — failures are swallowed.
pub fn save_progress() {
    let st = use_app_state();
    let Some(player) = st.player.get_untracked() else {
        return;
    };

    let a = audio();
    let progress = a.current_time().floor() as i32;
    if progress == LAST_SAVED_PROGRESS.load(Ordering::Relaxed) {
        return;
    }
    LAST_SAVED_PROGRESS.store(progress, Ordering::Relaxed);

    // Auto-mark done when we're within 30s of the end, unless the user
    // explicitly cleared "done" earlier in the session.
    let dur = a.duration();
    let auto_done = dur.is_finite() && dur > 0.0 && (dur - a.current_time() < AUTO_DONE_THRESHOLD_SECS);
    let done = if player.manual_done_off {
        false
    } else {
        player.item.done || auto_done
    };

    let episode_id = player.item.episode.id.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = api::report_progress(&episode_id, progress, done).await;
        // Mirror server-side progress into the in-memory player state so
        // any episode list rendered nearby reflects the new position.
        st.player.update(|opt| {
            if let Some(p) = opt.as_mut() {
                if p.item.episode.id == episode_id {
                    p.item.progress = Some(progress);
                    if done {
                        p.item.done = true;
                    }
                }
            }
        });
    });
}
