use tokio::sync::mpsc;

use crate::model::PodcastWithEpisodeStats;
use crate::tui::local_db::LocalDb;
use crate::tui::player::PlaybackState;

// ==============================================================================
// Actions — everything the app can do in response to events or async results
// ==============================================================================

pub enum Action {
    Quit,
    NavigateBack,

    // Login
    LoginFieldNext,
    LoginFieldPrev,
    LoginType(char),
    LoginBackspace,
    LoginSubmit,
    LoginResult(Result<String, String>),

    // Navigation
    ShowInbox,

    // Podcast list
    ListUp,
    ListDown,
    PageUp,
    PageDown,
    SelectPodcast,
    RefreshSync,
    SyncProgress(String),
    SyncComplete(Result<(), String>),
    PodcastsLoaded(Vec<PodcastWithEpisodeStats>),

    // Episode list
    SelectEpisode,
    EpisodesLoaded(Vec<EpisodeRow>),
    ToggleDone,

    // Episode detail
    ScrollUp,
    ScrollDown,

    // Playback
    PlayEpisode,
    TogglePause,
    SeekForward,
    SeekBackward,
    StopPlayback,
    PlaybackStarted(Result<(), String>),
    PlaybackUpdate(PlaybackState),
    PlaybackFinished,

    // Periodic progress push to server
    PushProgress,
    PushProgressComplete(Result<usize, String>),

    // Tick (periodic background processing)
    Tick,
}

// ==============================================================================
// View state
// ==============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginField {
    ServerUrl,
    Username,
    Password,
}

pub struct LoginState {
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub active_field: LoginField,
    pub error: Option<String>,
    pub loading: bool,
}

pub struct PodcastListState {
    pub podcasts: Vec<PodcastWithEpisodeStats>,
    pub selected: usize,
    pub loading: bool,
}

#[derive(Clone)]
pub struct EpisodeRow {
    pub id: String,
    pub podcast_id: String,
    pub title: String,
    pub publication_date: String,
    pub audio_url: String,
    pub audio_duration: i32,
    pub summary: String,
    pub content_encoded: String,
    pub progress: i32,
    pub done: bool,
    /// Set when displaying episodes across multiple podcasts (inbox view).
    pub podcast_title: Option<String>,
}

pub struct EpisodeListState {
    pub podcast_title: String,
    pub episodes: Vec<EpisodeRow>,
    pub selected: usize,
    pub loading: bool,
    /// Tick counter used for auto-scrolling long titles on the selected row.
    pub scroll_tick: usize,
}

pub struct EpisodeDetailState {
    pub episode: EpisodeRow,
    pub scroll: u16,
    /// Remembered so we can navigate back to the episode list.
    pub podcast_title: String,
    pub episode_index: usize,
}

pub struct InboxState {
    pub episodes: Vec<EpisodeRow>,
    pub selected: usize,
    pub scroll_tick: usize,
    /// Whether there are more episodes to load.
    pub has_more: bool,
}

const INBOX_PAGE_SIZE: i64 = 50;

pub enum View {
    Login(LoginState),
    Inbox(InboxState),
    PodcastList(PodcastListState),
    EpisodeList(EpisodeListState),
    EpisodeDetail(EpisodeDetailState),
}

// ==============================================================================
// App — central state machine
// ==============================================================================

/// Playback state visible to the UI.
pub struct NowPlaying {
    pub episode_id: String,
    pub episode_title: String,
    pub state: PlaybackState,
}

pub struct App {
    pub view: View,
    pub should_quit: bool,
    pub status_message: Option<String>,
    pub syncing: bool,
    pub sync_status: Option<String>,
    pub action_tx: mpsc::UnboundedSender<Action>,
    pub action_rx: mpsc::UnboundedReceiver<Action>,
    pub db: LocalDb,
    pub now_playing: Option<NowPlaying>,
}

impl App {
    pub fn new(db: LocalDb) -> Self {
        let (action_tx, action_rx) = mpsc::unbounded_channel();

        // If we already have a token stored, go straight to the inbox.
        let has_token = db.get_config("auth_token").is_some();

        let view = if has_token {
            let episodes = db.list_inbox_episodes(INBOX_PAGE_SIZE, 0);
            let has_more = episodes.len() as i64 >= INBOX_PAGE_SIZE;
            View::Inbox(InboxState {
                episodes,
                selected: 0,
                scroll_tick: 0,
                has_more,
            })
        } else {
            let server_url = db
                .get_config("server_url")
                .unwrap_or_else(|| "http://localhost:3000".to_string());
            View::Login(LoginState {
                server_url,
                username: String::new(),
                password: String::new(),
                active_field: LoginField::ServerUrl,
                error: None,
                loading: false,
            })
        };

        Self {
            view,
            should_quit: false,
            status_message: None,
            syncing: false,
            sync_status: None,
            action_tx,
            action_rx,
            db,
            now_playing: None,
        }
    }

    /// Process an action, updating state accordingly.
    pub fn update(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Tick => {
                // Advance scroll tick for auto-scrolling long titles.
                match self.view {
                    View::EpisodeList(ref mut s) => {
                        s.scroll_tick = s.scroll_tick.wrapping_add(1);
                    }
                    View::Inbox(ref mut s) => {
                        s.scroll_tick = s.scroll_tick.wrapping_add(1);
                    }
                    _ => {}
                }
            }

            // Login actions
            Action::LoginFieldNext => {
                if let View::Login(ref mut s) = self.view {
                    s.active_field = match s.active_field {
                        LoginField::ServerUrl => LoginField::Username,
                        LoginField::Username => LoginField::Password,
                        LoginField::Password => LoginField::ServerUrl,
                    };
                }
            }
            Action::LoginFieldPrev => {
                if let View::Login(ref mut s) = self.view {
                    s.active_field = match s.active_field {
                        LoginField::ServerUrl => LoginField::Password,
                        LoginField::Username => LoginField::ServerUrl,
                        LoginField::Password => LoginField::Username,
                    };
                }
            }
            Action::LoginType(ch) => {
                if let View::Login(ref mut s) = self.view {
                    match s.active_field {
                        LoginField::ServerUrl => s.server_url.push(ch),
                        LoginField::Username => s.username.push(ch),
                        LoginField::Password => s.password.push(ch),
                    }
                }
            }
            Action::LoginBackspace => {
                if let View::Login(ref mut s) = self.view {
                    match s.active_field {
                        LoginField::ServerUrl => { s.server_url.pop(); }
                        LoginField::Username => { s.username.pop(); }
                        LoginField::Password => { s.password.pop(); }
                    }
                }
            }
            Action::LoginSubmit => {
                // Loading state is set in handle_async_action before
                // spawning the request, to prevent double-submit races.
            }
            Action::LoginResult(Ok(username)) => {
                self.status_message = Some(format!("Logged in as {}", username));
                self.view = View::Inbox(self.new_inbox_state());
                let _ = self.action_tx.send(Action::RefreshSync);
            }
            Action::LoginResult(Err(e)) => {
                if let View::Login(ref mut s) = self.view {
                    s.error = Some(e);
                    s.loading = false;
                }
            }

            Action::ShowInbox => {
                self.view = View::Inbox(self.new_inbox_state());
            }

            // Navigation
            Action::NavigateBack => {
                match self.view {
                    View::EpisodeDetail(ref s) => {
                        let podcast_id = s.episode.podcast_id.clone();
                        let podcast_title = s.podcast_title.clone();
                        let selected = s.episode_index;
                        let episodes = self.db.list_episodes(&podcast_id);
                        self.view = View::EpisodeList(EpisodeListState {
                            podcast_title,
                            episodes,
                            selected,
                            loading: false,
                            scroll_tick: 0,
                        });
                    }
                    View::EpisodeList(_) | View::Inbox(_) => {
                        self.load_podcasts();
                    }
                    _ => {}
                }
            }

            // List navigation
            Action::ListUp => match self.view {
                View::PodcastList(ref mut s) => {
                    if s.selected > 0 { s.selected -= 1; }
                }
                View::EpisodeList(ref mut s) => {
                    if s.selected > 0 { s.selected -= 1; s.scroll_tick = 0; }
                }
                View::Inbox(ref mut s) => {
                    if s.selected > 0 { s.selected -= 1; s.scroll_tick = 0; }
                }
                _ => {}
            },
            Action::ListDown => match self.view {
                View::PodcastList(ref mut s) => {
                    if s.selected + 1 < s.podcasts.len() { s.selected += 1; }
                }
                View::EpisodeList(ref mut s) => {
                    if s.selected + 1 < s.episodes.len() { s.selected += 1; s.scroll_tick = 0; }
                }
                View::Inbox(ref mut s) => {
                    if s.selected + 1 < s.episodes.len() { s.selected += 1; s.scroll_tick = 0; }
                }
                _ => {}
            },
            // After ListDown, check if we need to load more inbox episodes.

            Action::PageUp => match self.view {
                View::PodcastList(ref mut s) => {
                    s.selected = s.selected.saturating_sub(10);
                }
                View::EpisodeList(ref mut s) => {
                    s.selected = s.selected.saturating_sub(10);
                    s.scroll_tick = 0;
                }
                View::Inbox(ref mut s) => {
                    s.selected = s.selected.saturating_sub(10);
                    s.scroll_tick = 0;
                }
                View::EpisodeDetail(ref mut s) => {
                    s.scroll = s.scroll.saturating_sub(10);
                }
                _ => {}
            },
            Action::PageDown => match self.view {
                View::PodcastList(ref mut s) => {
                    s.selected = (s.selected + 10).min(s.podcasts.len().saturating_sub(1));
                }
                View::EpisodeList(ref mut s) => {
                    s.selected = (s.selected + 10).min(s.episodes.len().saturating_sub(1));
                    s.scroll_tick = 0;
                }
                View::Inbox(ref mut s) => {
                    s.selected = (s.selected + 10).min(s.episodes.len().saturating_sub(1));
                    s.scroll_tick = 0;
                }
                View::EpisodeDetail(ref mut s) => {
                    s.scroll = s.scroll.saturating_add(10);
                }
                _ => {}
            },

            // Podcast selection
            Action::SelectPodcast => {
                if let View::PodcastList(ref s) = self.view {
                    if let Some(podcast) = s.podcasts.get(s.selected) {
                        let episodes = self.db.list_episodes(&podcast.id);
                        self.view = View::EpisodeList(EpisodeListState {
                            podcast_title: podcast.title.clone(),
                            episodes,
                            selected: 0,
                            scroll_tick: 0,
                            loading: false,
                        });
                    }
                }
            }

            // Episode selection — works from EpisodeList and Inbox.
            Action::SelectEpisode => {
                let detail = match &self.view {
                    View::EpisodeList(s) => s.episodes.get(s.selected).map(|e| {
                        (e.clone(), s.podcast_title.clone(), s.selected)
                    }),
                    View::Inbox(s) => s.episodes.get(s.selected).map(|e| {
                        (e.clone(), e.podcast_title.clone().unwrap_or_default(), s.selected)
                    }),
                    _ => None,
                };
                if let Some((episode, podcast_title, index)) = detail {
                    self.view = View::EpisodeDetail(EpisodeDetailState {
                        episode,
                        scroll: 0,
                        podcast_title,
                        episode_index: index,
                    });
                }
            }

            // Scroll in detail view
            Action::ScrollUp => {
                if let View::EpisodeDetail(ref mut s) = self.view {
                    s.scroll = s.scroll.saturating_sub(1);
                }
            }
            Action::ScrollDown => {
                if let View::EpisodeDetail(ref mut s) = self.view {
                    s.scroll = s.scroll.saturating_add(1);
                }
            }

            // Toggle done on selected episode
            Action::ToggleDone => {
                match self.view {
                    View::EpisodeList(ref mut s) => {
                        if let Some(episode) = s.episodes.get_mut(s.selected) {
                            episode.done = !episode.done;
                            self.db.upsert_progress(
                                &episode.id, episode.progress, episode.done, true,
                            );
                            // Advance to the next episode.
                            if s.selected + 1 < s.episodes.len() {
                                s.selected += 1;
                                s.scroll_tick = 0;
                            }
                        }
                    }
                    View::Inbox(ref mut s) => {
                        if let Some(episode) = s.episodes.get(s.selected) {
                            let done = !episode.done;
                            self.db.upsert_progress(
                                &episode.id, episode.progress, done, true,
                            );
                            if done {
                                // Remove from inbox since it filters out done
                                // episodes. The cursor stays in place, which
                                // effectively selects the next episode.
                                s.episodes.remove(s.selected);
                                if s.selected >= s.episodes.len() && s.selected > 0 {
                                    s.selected -= 1;
                                }
                            } else {
                                s.episodes.get_mut(s.selected).unwrap().done = false;
                            }
                            s.scroll_tick = 0;
                        }
                    }
                    _ => {}
                }
            }

            // Sync
            Action::RefreshSync => {
                self.syncing = true;
                self.sync_status = Some("Syncing…".to_string());
                // Handled by event layer — spawns async sync task.
            }
            Action::SyncProgress(msg) => {
                self.sync_status = Some(msg);
            }
            Action::SyncComplete(Ok(())) => {
                self.syncing = false;
                self.sync_status = None;
                self.status_message = Some("Sync complete".to_string());
                self.reload_current_view();
            }
            Action::SyncComplete(Err(e)) => {
                self.syncing = false;
                self.sync_status = None;
                self.status_message = Some(format!("Sync error: {}", e));
                self.reload_current_view();
            }

            Action::PodcastsLoaded(podcasts) => {
                if let View::PodcastList(ref mut s) = self.view {
                    s.podcasts = podcasts;
                    s.loading = false;
                }
            }

            Action::EpisodesLoaded(episodes) => {
                if let View::EpisodeList(ref mut s) = self.view {
                    s.episodes = episodes;
                    s.loading = false;
                }
            }

            // Playback
            Action::PlayEpisode => {
                // Set now_playing from current view before event layer spawns mpv.
                let episode = match &self.view {
                    View::EpisodeList(s) => s.episodes.get(s.selected).cloned(),
                    View::Inbox(s) => s.episodes.get(s.selected).cloned(),
                    View::EpisodeDetail(s) => Some(s.episode.clone()),
                    _ => None,
                };
                if let Some(ep) = episode {
                    self.now_playing = Some(NowPlaying {
                        episode_id: ep.id.clone(),
                        episode_title: ep.title.clone(),
                        state: PlaybackState {
                            position_secs: ep.progress,
                            duration_secs: ep.audio_duration,
                            paused: false,
                            finished: false,
                        },
                    });
                    self.status_message = Some(format!("Starting: {}", ep.title));
                }
            }
            Action::TogglePause | Action::SeekForward | Action::SeekBackward => {
                // Handled by event layer — sends IPC commands.
            }
            Action::StopPlayback => {
                // Handled by event layer — kills mpv.
                self.now_playing = None;
                self.status_message = Some("Playback stopped".to_string());
                let _ = self.action_tx.send(Action::PushProgress);
            }
            Action::PlaybackStarted(Ok(())) => {
                self.status_message = Some("Playing".to_string());
            }
            Action::PlaybackStarted(Err(e)) => {
                self.now_playing = None;
                self.status_message = Some(format!("Playback error: {}", e));
            }
            Action::PlaybackUpdate(state) => {
                if let Some(ref mut np) = self.now_playing {
                    // Persist progress locally.
                    if state.position_secs > 0 {
                        self.db.upsert_progress(
                            &np.episode_id,
                            state.position_secs,
                            state.finished,
                            true,
                        );
                    }
                    np.state = state;
                }
            }
            Action::PushProgress => {
                // Handled by event layer.
            }
            Action::PushProgressComplete(Ok(count)) => {
                if count > 0 {
                    self.status_message = Some(format!("Synced {} progress update(s)", count));
                }
            }
            Action::PushProgressComplete(Err(e)) => {
                self.status_message = Some(format!("Progress sync error: {}", e));
            }
            Action::PlaybackFinished => {
                if let Some(ref np) = self.now_playing {
                    self.db.upsert_progress(&np.episode_id, np.state.position_secs, true, true);
                }
                self.now_playing = None;
                self.status_message = Some("Playback finished".to_string());
                let _ = self.action_tx.send(Action::PushProgress);
            }
        }

        // Auto-load more inbox episodes when scrolling near the end.
        self.maybe_load_more_inbox();
    }

    /// Reload the current view's data from the local database.
    fn reload_current_view(&mut self) {
        match self.view {
            View::PodcastList(ref mut s) => {
                s.podcasts = self.db.list_podcasts();
                s.loading = false;
            }
            View::Inbox(ref mut s) => {
                // Reload keeping at least as many episodes as currently loaded.
                let count = (s.episodes.len() as i64).max(INBOX_PAGE_SIZE);
                s.episodes = self.db.list_inbox_episodes(count, 0);
                s.has_more = s.episodes.len() as i64 >= count;
            }
            _ => {}
        }
    }

    fn new_inbox_state(&self) -> InboxState {
        let episodes = self.db.list_inbox_episodes(INBOX_PAGE_SIZE, 0);
        let has_more = episodes.len() as i64 >= INBOX_PAGE_SIZE;
        InboxState {
            episodes,
            selected: 0,
            scroll_tick: 0,
            has_more,
        }
    }

    /// Load more episodes into the inbox when the user scrolls near the end.
    fn maybe_load_more_inbox(&mut self) {
        if let View::Inbox(ref mut s) = self.view {
            if !s.has_more {
                return;
            }
            // Load more when within 5 items of the end.
            if s.selected + 5 >= s.episodes.len() {
                let offset = s.episodes.len() as i64;
                let more = self.db.list_inbox_episodes(INBOX_PAGE_SIZE, offset);
                s.has_more = more.len() as i64 >= INBOX_PAGE_SIZE;
                s.episodes.extend(more);
            }
        }
    }

    fn load_podcasts(&mut self) {
        let podcasts = self.db.list_podcasts();
        self.view = View::PodcastList(PodcastListState {
            podcasts,
            selected: 0,
            loading: false,
        });
    }
}
