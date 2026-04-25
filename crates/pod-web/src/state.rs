// ==============================================================================
// Global app state
// ==============================================================================
//
// The app keeps all shared reactive state in a single `AppState` struct that
// is provided via Leptos's `provide_context` at the root and pulled with
// `use_app_state` from anywhere in the tree.
//
// The token + username are mirrored into `localStorage` so the session
// survives reloads; everything else is in-memory.

use gloo_storage::{LocalStorage, Storage};
use leptos::prelude::*;
use pod_model::{EpisodeWithProgress, InboxEpisode, Podcast, PodcastWithEpisodeStats};

const TOKEN_KEY: &str = "pod_token";
const USERNAME_KEY: &str = "pod_username";

/// What the player bar is currently bound to.
///
/// `podcast` is intentionally a thin context object rather than a full
/// `Podcast` so we can populate it from the inbox view (which only knows the
/// podcast id / title / image).
#[derive(Clone)]
pub struct PlayerItem {
    pub item: EpisodeWithProgress,
    pub podcast: PodcastCtx,
    pub playing: bool,
    /// Set when the user explicitly toggles "done" off so auto-save won't
    /// flip it back on as the episode finishes.
    pub manual_done_off: bool,
}

#[derive(Clone)]
pub struct PodcastCtx {
    pub id: String,
    pub title: String,
    pub image_link: String,
}

impl From<&Podcast> for PodcastCtx {
    fn from(p: &Podcast) -> Self {
        PodcastCtx {
            id: p.id.clone(),
            title: p.title.clone(),
            image_link: p.image_link.clone(),
        }
    }
}

impl From<&PodcastWithEpisodeStats> for PodcastCtx {
    fn from(p: &PodcastWithEpisodeStats) -> Self {
        PodcastCtx {
            id: p.id.clone(),
            title: p.title.clone(),
            image_link: p.image_link.clone(),
        }
    }
}

impl From<&InboxEpisode> for PodcastCtx {
    fn from(item: &InboxEpisode) -> Self {
        PodcastCtx {
            id: item.episode.podcast_id.clone(),
            title: item.podcast_title.clone(),
            image_link: item.podcast_image_link.clone(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct AppState {
    pub token: RwSignal<Option<String>>,
    pub username: RwSignal<String>,
    pub podcasts: RwSignal<Vec<PodcastWithEpisodeStats>>,
    pub player: RwSignal<Option<PlayerItem>>,
    pub breadcrumb: RwSignal<String>,
}

impl AppState {
    fn load() -> Self {
        let token: Option<String> = LocalStorage::get(TOKEN_KEY).ok();
        let username: String = LocalStorage::get(USERNAME_KEY).unwrap_or_default();
        Self {
            token: RwSignal::new(token),
            username: RwSignal::new(username),
            podcasts: RwSignal::new(Vec::new()),
            player: RwSignal::new(None),
            breadcrumb: RwSignal::new(String::new()),
        }
    }

    /// Persist a fresh login.
    pub fn set_session(&self, token: String, username: String) {
        let _ = LocalStorage::set(TOKEN_KEY, &token);
        let _ = LocalStorage::set(USERNAME_KEY, &username);
        self.token.set(Some(token));
        self.username.set(username);
    }

    /// Wipe local credentials and in-memory state. Used on explicit logout
    /// AND on 401 responses from the server.
    pub fn force_logout(&self) {
        LocalStorage::delete(TOKEN_KEY);
        LocalStorage::delete(USERNAME_KEY);
        self.token.set(None);
        self.username.set(String::new());
        self.podcasts.set(Vec::new());
        self.player.set(None);
        self.breadcrumb.set(String::new());
        crate::player::stop_audio();
    }
}

pub fn provide_app_state() {
    provide_context(AppState::load());
}

/// Pull the shared `AppState`. Panics if called outside the `App` tree, which
/// shouldn't happen in practice.
pub fn use_app_state() -> AppState {
    use_context::<AppState>().expect("AppState must be provided at the root")
}
