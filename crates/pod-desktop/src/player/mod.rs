//! The player (spec 8): one entity that owns the mpv engine, the episode
//! in the player bar and the playback contract for it.
//!
//! The entity lives on the UI thread and never blocks: mpv sits behind the
//! channels of [`mpv::Engine`], and everything that touches the store runs
//! on the tokio side in one worker task, in the order the player asked.
//! That order matters: a play that follows a seek or a finished episode
//! must read the state those writes left, and position saves for one
//! episode must land oldest first.
//!
//! The contract itself (start position, replay reset, the 5 s saves, the
//! played threshold) is the pure [`contract`] module; this one feeds it
//! the player's events and turns its writes into local writes (spec 5.2).
//!
//! The player is also this device's part in Connect (spec 14, [`output`]):
//! it reports the local play to the server, runs the commands other
//! devices send it, and while another device is active it follows that
//! device's playback. mpv is stopped then and the player writes nothing:
//! its buttons send commands to the active device, which writes the
//! listening state.

pub mod contract;
pub mod media_keys;
pub mod mpv;
mod output;
pub mod settings;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use pod_protocol::{
    ConnectCommand, ConnectDevice, EpisodeId, Playback, PlaybackEpisode, PodcastId, SonosPlayer,
};
use rusqlite::{Connection, OptionalExtension as _};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use self::contract::{Contract, Stored, Write};
use self::mpv::{Command, Event, Failure};
use self::output::{Report, Stream};
use self::settings::{PlayerSettings, SKIP_STEPS_MS, SPEEDS, next_step};
use crate::connect::stream::{self, Link};
use crate::connect::{self, Remote, Target};
use crate::reconcile::Reconciler;
use crate::runtime::Background;
use crate::store::{Store, Table, live};
use crate::sync::SyncHandle;
use crate::sync::engine::SyncEngine;
use crate::sync::local_write::{self, EpisodeChange, LocalChange};

/// The `device_setting` key that remembers the episode in the player bar
/// across restarts (spec 8).
pub const LAST_EPISODE_ID: &str = "last_episode_id";

/// How long quitting waits for the last position write.
pub const QUIT_FLUSH: Duration = Duration::from_secs(1);

// The player's commands, for the player bar and for anything else that
// controls playback, such as media keys. They are handled app-wide, so
// they work whichever view has focus.
gpui_kit::actions!(
    player,
    [
        TogglePlayback,
        SkipBack,
        SkipForward,
        CycleSpeed,
        CycleSkipBack,
        CycleSkipForward
    ]
);

/// The app's player, as a global so any view (and later the media keys)
/// can reach it.
#[derive(Clone, Debug)]
pub struct PlayerHandle(pub Entity<Player>);

impl Global for PlayerHandle {}

/// Registers the app-wide handlers of the player's actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &TogglePlayback, cx| update(cx, Player::toggle));
    cx.on_action(|_: &SkipBack, cx| update(cx, |player, cx| player.skip(false, cx)));
    cx.on_action(|_: &SkipForward, cx| update(cx, |player, cx| player.skip(true, cx)));
    cx.on_action(|_: &CycleSpeed, cx| update(cx, Player::cycle_speed));
    cx.on_action(|_: &CycleSkipBack, cx| update(cx, |player, cx| player.cycle_skip(false, cx)));
    cx.on_action(|_: &CycleSkipForward, cx| update(cx, |player, cx| player.cycle_skip(true, cx)));
}

/// Plays `episode`, or resumes it if it is the one in the player.
pub fn play(episode: EpisodeId, cx: &mut App) {
    update(cx, |player, cx| player.play(episode, cx));
}

/// Plays `episode` from `at_ms`, as a chapter row does (spec 17.4): a seek
/// when it is the loaded episode, otherwise a start there.
pub fn play_at(episode: EpisodeId, at_ms: i64, cx: &mut App) {
    update(cx, |player, cx| player.play_at(episode, at_ms, cx));
}

/// Pauses `episode` if it is playing, otherwise plays or resumes it: what
/// an episode's play button does.
pub fn play_or_pause(episode: EpisodeId, cx: &mut App) {
    update(cx, |player, cx| match player.current() {
        Some(current) if current.episode.id == episode && current.is_playing() => player.pause(cx),
        _ => player.play(episode, cx),
    });
}

/// The episode in the player, whether it plays, and where it stands, for
/// the views that mark it.
pub fn now_playing(cx: &App) -> Option<(EpisodeId, bool, i64)> {
    let PlayerHandle(player) = cx.try_global::<PlayerHandle>()?;
    let current = player.read(cx).current()?;
    Some((
        current.episode.id,
        current.is_playing(),
        current.position_ms(),
    ))
}

fn update(cx: &mut App, change: impl FnOnce(&mut Player, &mut Context<Player>)) {
    if let Some(PlayerHandle(player)) = cx.try_global::<PlayerHandle>().cloned() {
        player.update(cx, change);
    }
}

// ==============================================================================
// The episode in the player
// ==============================================================================

/// What the player knows about an episode, read once when it is loaded.
/// A sync can delete the episode row while it plays (spec 8); the player
/// carries on with this copy, and its writes go to `episode_state`, which
/// has no foreign key to `episode`.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub id: EpisodeId,
    pub podcast_id: PodcastId,
    pub title: String,
    pub podcast_title: String,
    /// The episode's artwork, else the show's.
    pub artwork_url: Option<String>,
    /// The cached `artwork.file` of the episode's artwork, else of the
    /// show's, when it was cached as the episode loaded.
    pub artwork_file: Option<String>,
    /// The feed's duration, until mpv reports the file's.
    pub duration_ms: Option<i64>,
    pub source: Source,
    pub stored: Stored,
}

/// Where the audio comes from: the downloaded file when its `download` row
/// is `complete` and the file exists, else the stream (spec 8). Playing
/// never starts a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Download(PathBuf),
    Stream(String),
    /// Known only from the Connect playback (spec 14.1): the store has no
    /// row for the episode, so it can't play here. Every play reads the
    /// store again, so one that a sync brought meanwhile plays.
    Elsewhere,
}

impl Source {
    fn choose(downloaded: Option<PathBuf>, audio_url: String) -> Source {
        match downloaded {
            Some(path) if path.is_file() => Source::Download(path),
            _ => Source::Stream(audio_url),
        }
    }

    fn for_mpv(&self) -> String {
        match self {
            Source::Download(path) => path.display().to_string(),
            Source::Stream(url) => url.clone(),
            Source::Elsewhere => String::new(),
        }
    }
}

impl Episode {
    /// The playback's episode, for a device that doesn't have it locally,
    /// or until the store's row is read.
    fn from_playback(episode: &PlaybackEpisode) -> Episode {
        Episode {
            id: episode.id,
            podcast_id: episode.podcast_id,
            title: episode.title.clone(),
            podcast_title: episode.podcast_title.clone().unwrap_or_default(),
            artwork_url: episode.artwork_url.clone(),
            artwork_file: None,
            duration_ms: episode.duration_ms,
            source: Source::Elsewhere,
            stored: Stored {
                position_ms: 0,
                played: false,
            },
        }
    }
}

/// Reads `id` for the player, with the source resolved against the
/// downloads `root`. `None` when the episode isn't in the store. Checks
/// the file on disk, so it runs off the UI thread.
fn load_episode(connection: &Connection, id: EpisodeId, root: &Path) -> Result<Option<Episode>> {
    let row = connection
        .query_row(
            "select e.podcast_id, e.title, coalesce(p.title, p.local_title, p.feed_url, ''),
                    e.duration_ms, e.audio_url, coalesce(s.position_ms, 0),
                    coalesce(s.played, 0), d.path,
                    coalesce(e.artwork_url, p.artwork_url, p.local_artwork_url),
                    coalesce(
                        (select file from artwork where url = e.artwork_url),
                        (select file from artwork
                         where url = coalesce(p.artwork_url, p.local_artwork_url))
                    )
             from episode e
             left join podcast p on p.id = e.podcast_id
             left join episode_state s on s.episode_id = e.id
             left join download d on d.episode_id = e.id and d.state = 'complete'
             where e.id = ?1",
            [id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()
        .context("query the episode")?;
    let Some((
        podcast_id,
        title,
        podcast_title,
        duration_ms,
        audio_url,
        position_ms,
        played,
        path,
        artwork_url,
        artwork_file,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(Episode {
        id,
        podcast_id: podcast_id.parse().context("parse the podcast id")?,
        title,
        podcast_title,
        artwork_url,
        artwork_file,
        duration_ms,
        source: Source::choose(path.map(|path| root.join(path)), audio_url),
        stored: Stored {
            position_ms,
            played,
        },
    }))
}

/// The episode remembered in [`LAST_EPISODE_ID`], if it still exists
/// locally. One that doesn't is dropped (spec 8).
fn load_last_episode(connection: &Connection, root: &Path) -> Result<Option<Episode>> {
    let value: Option<String> = connection
        .query_row(
            "select value from device_setting where key = ?1",
            [LAST_EPISODE_ID],
            |row| row.get(0),
        )
        .optional()
        .context("read the last episode id")?;
    let id = value
        .and_then(|value| serde_json::from_str::<String>(&value).ok())
        .and_then(|id| id.parse::<EpisodeId>().ok());
    match id {
        Some(id) => load_episode(connection, id, root).context("read the last episode"),
        None => Ok(None),
    }
}

// ==============================================================================
// The store worker
// ==============================================================================

/// Work for the store, done one job at a time on the tokio side.
#[derive(Debug)]
enum Job {
    Write(LocalChange),
    /// Remember the episode for the next start.
    Remember(EpisodeId),
    /// Read an episode, or with `None` the remembered one.
    Load {
        id: Option<EpisodeId>,
        reply: oneshot::Sender<Option<Episode>>,
    },
    /// Answer once the jobs queued before it are done.
    Barrier(oneshot::Sender<()>),
    /// Stop after the jobs queued so far, as the app quits.
    Finish,
}

async fn work(
    store: Store,
    sync: SyncHandle,
    root: PathBuf,
    mut jobs: mpsc::UnboundedReceiver<Job>,
) {
    while let Some(job) = jobs.recv().await {
        match job {
            Job::Write(change) => {
                if let Err(error) = local_write::write(&store, &sync, change.clone()).await {
                    // TODO: tell the user, once the window has a place for errors.
                    tracing::error!(?change, "player write failed: {error:#}");
                }
            }
            Job::Remember(id) => {
                let remembered = store
                    .write(move |transaction| {
                        transaction
                            .execute(
                                "insert into device_setting (key, value) values (?1, ?2)
                                 on conflict (key) do update set value = excluded.value",
                                [
                                    LAST_EPISODE_ID,
                                    &serde_json::json!(id.to_string()).to_string(),
                                ],
                            )
                            .context("write the last episode id")
                    })
                    .await;
                if let Err(error) = remembered {
                    tracing::error!("remembering episode {id} failed: {error:#}");
                }
            }
            Job::Load { id, reply } => {
                let root = root.clone();
                let episode = store
                    .read(move |connection| match id {
                        Some(id) => load_episode(connection, id, &root),
                        None => load_last_episode(connection, &root),
                    })
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?id, "reading the episode to play failed: {error:#}");
                        None
                    });
                let _ = reply.send(episode);
            }
            Job::Barrier(reply) => {
                let _ = reply.send(());
            }
            Job::Finish => return,
        }
    }
}

// ==============================================================================
// The player entity
// ==============================================================================

/// The episode in the player bar.
#[derive(Debug)]
pub struct Current {
    pub episode: Episode,
    /// Where the episode stands while mpv hasn't loaded it: restored on
    /// start, or after it ended, stopped or failed.
    parked_ms: i64,
    phase: Phase,
}

/// Whether mpv has the episode, and how its last play went.
#[derive(Debug)]
pub(crate) enum Phase {
    /// Not loaded: restored on start, or after the play ended or stopped.
    Parked,
    /// mpv has the file (playing or paused); the contract tracks the play.
    Loaded(Contract),
    /// mpv couldn't play it. Play tries again.
    Failed(Failure),
    /// Another Connect device is active (spec 14.7). mpv is stopped and
    /// the player writes nothing.
    Remote {
        remote: Remote,
        /// Tells the answers to commands of this remote mode from a later
        /// one's.
        generation: u64,
        /// Runs the clock. Dropping it stops it.
        _clock: Task<()>,
    },
}

impl Current {
    pub(crate) fn new(episode: Episode, parked_ms: i64, phase: Phase) -> Self {
        Current {
            episode,
            parked_ms,
            phase,
        }
    }

    pub fn position_ms(&self) -> i64 {
        match &self.phase {
            Phase::Loaded(session) => session.position_ms(),
            Phase::Remote { remote, .. } => remote.position_ms(),
            Phase::Parked | Phase::Failed(_) => self.parked_ms,
        }
    }

    pub fn duration_ms(&self) -> Option<i64> {
        self.session()
            .and_then(Contract::duration_ms)
            .or(self.episode.duration_ms)
    }

    pub fn is_playing(&self) -> bool {
        match &self.phase {
            Phase::Loaded(session) => session.is_playing(),
            Phase::Remote { remote, .. } => remote.is_playing(),
            Phase::Parked | Phase::Failed(_) => false,
        }
    }

    /// Why the last play failed, until the next one starts.
    pub fn failure(&self) -> Option<Failure> {
        match self.phase {
            Phase::Failed(failure) => Some(failure),
            Phase::Parked | Phase::Loaded(_) | Phase::Remote { .. } => None,
        }
    }

    /// Another device's playback, while that device is active.
    pub fn remote(&self) -> Option<&Remote> {
        match &self.phase {
            Phase::Remote { remote, .. } => Some(remote),
            Phase::Parked | Phase::Loaded(_) | Phase::Failed(_) => None,
        }
    }

    fn session(&self) -> Option<&Contract> {
        match &self.phase {
            Phase::Loaded(session) => Some(session),
            Phase::Parked | Phase::Failed(_) | Phase::Remote { .. } => None,
        }
    }

    pub(crate) fn session_mut(&mut self) -> Option<&mut Contract> {
        match &mut self.phase {
            Phase::Loaded(session) => Some(session),
            Phase::Parked | Phase::Failed(_) | Phase::Remote { .. } => None,
        }
    }
}

/// A Connect request failed in a way the user should hear about, or an
/// episode can't play here. The app shows it as a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectNotice(pub String);

pub struct Player {
    commands: mpsc::UnboundedSender<Command>,
    jobs: mpsc::UnboundedSender<Job>,
    worker: Option<JoinHandle<()>>,
    reconciler: Reconciler,
    /// Pushes the outbox before a transfer the server starts from its own
    /// copy of the listening state.
    engine: SyncEngine,
    connect: connect::Api,
    /// The stream's client, which has no overall timeout.
    stream_http: reqwest::Client,
    /// Sends the reports one at a time, in the order they were made.
    reports: mpsc::UnboundedSender<Report>,
    /// The account the event stream speaks for.
    link: Option<Link>,
    /// The event stream of `link`.
    stream: Option<Stream>,
    /// A `state` arrived on the stream's current connection, so reports go
    /// out (spec 14.3).
    stream_open: bool,
    /// Counts `state` events, so an answer sent before the latest one
    /// changes nothing.
    states: u64,
    /// The account's connected client devices, this one included.
    devices: Vec<ConnectDevice>,
    /// The account's playback, as last heard.
    playback: Option<Playback>,
    /// The Sonos groups, as last listed. The output picker only shows them
    /// when there are any.
    sonos_players: Vec<SonosPlayer>,
    /// A transfer waiting for its answer.
    transfer: Option<Target>,
    /// Counts remote modes, for [`Phase::Remote`]'s `generation`.
    outputs: u64,
    settings: PlayerSettings,
    current: Option<Current>,
    /// Loads sent to mpv whose start it hasn't reported. Until it has,
    /// mpv's events are about the file being replaced.
    pending_starts: usize,
    /// Counts play requests, so only the newest one's load starts playing.
    requests: u64,
    /// The play request reading its episode from the store, which counts as
    /// playing here: the playback it starts mustn't be replaced meanwhile.
    loading: Option<u64>,
    _events: Task<()>,
    _settings: Task<()>,
    _account: Task<()>,
    _heartbeat: Task<()>,
}

impl EventEmitter<ConnectNotice> for Player {}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("settings", &self.settings)
            .field("current", &self.current)
            .field("pending_starts", &self.pending_starts)
            .field("stream_open", &self.stream_open)
            .field("playback", &self.playback)
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Requires the [`Store`], [`SyncHandle`], [`SyncEngine`],
    /// [`Background`] and [`Reconciler`] globals. Restores the remembered
    /// episode, paused, until the event stream brings the account's
    /// playback.
    pub fn new(engine: mpv::Engine, cx: &mut Context<Self>) -> Self {
        let reconciler = cx.global::<Reconciler>().clone();
        let sync_engine = cx.global::<SyncEngine>().clone();
        let background = cx.global::<Background>().clone();
        let connect = connect::Api::new(cx.global::<Store>().clone(), sync_engine.http().clone());
        let stream_http = stream::client().unwrap_or_else(|error| {
            tracing::error!("{error:#}; the event stream uses the sync client");
            sync_engine.http().clone()
        });
        let (jobs, job_receiver) = mpsc::unbounded_channel();
        let worker = background.spawn(work(
            cx.global::<Store>().clone(),
            cx.global::<SyncHandle>().clone(),
            reconciler.root().to_owned(),
            job_receiver,
        ));
        let (reports, report_receiver) = mpsc::unbounded_channel();
        background.spawn(output::send_reports(connect.clone(), report_receiver));

        let mut events = engine.events;
        let events = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.on_event(event, cx))
                    .is_err()
                {
                    return;
                }
            }
        });

        let player = Player {
            commands: engine.commands,
            jobs,
            worker: Some(worker),
            reconciler,
            engine: sync_engine,
            connect,
            stream_http,
            reports,
            link: None,
            stream: None,
            stream_open: false,
            states: 0,
            devices: Vec::new(),
            playback: None,
            sonos_players: Vec::new(),
            transfer: None,
            outputs: 0,
            settings: PlayerSettings::default(),
            current: None,
            pending_starts: 0,
            requests: 0,
            loading: None,
            _events: events,
            _settings: live::observe_tables(
                cx,
                Table::Setting.into(),
                PlayerSettings::load,
                |this: &mut Self, settings, cx| this.apply_settings(settings, cx),
            ),
            // Signing in, out or again opens the stream for the account.
            _account: live::observe_tables(
                cx,
                Table::Account.into(),
                stream::load_link,
                |this: &mut Self, link, cx| this.set_link(link, cx),
            ),
            _heartbeat: output::heartbeat(background, cx),
        };

        let restored = player.load(None);
        cx.spawn(async move |this, cx| {
            let Ok(Some(episode)) = restored.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                // A play, or the account's playback, that came meanwhile
                // wins over the restore.
                if this.current.is_none() && this.requests == 0 {
                    let parked_ms = episode.stored.start_position_ms();
                    this.current = Some(Current::new(episode, parked_ms, Phase::Parked));
                    cx.notify();
                }
            });
        })
        .detach();
        player
    }

    pub fn current(&self) -> Option<&Current> {
        self.current.as_ref()
    }

    pub fn settings(&self) -> PlayerSettings {
        self.settings
    }

    /// Plays `id` from its stored position, or resumes it if it is the
    /// loaded episode. While another device is active, it plays there.
    pub fn play(&mut self, id: EpisodeId, cx: &mut Context<Self>) {
        if let Some(current) = &self.current
            && let Some(remote) = current.remote()
        {
            if current.episode.id != id {
                self.command_remote(ConnectCommand::Play { episode_id: id }, cx);
            } else if !remote.is_playing() {
                self.command_remote(ConnectCommand::Resume, cx);
            }
            return;
        }
        self.play_here(id, None, cx);
    }

    /// Plays `id` from `at_ms`, and keeps playing. While another device is
    /// active, it seeks there when that device has `id` loaded.
    pub fn play_at(&mut self, id: EpisodeId, at_ms: i64, cx: &mut Context<Self>) {
        if let Some(current) = &self.current
            && let Some(remote) = current.remote()
        {
            if current.episode.id == id {
                let resume = !remote.is_playing();
                self.seek(at_ms, cx);
                if resume {
                    self.command_remote(ConnectCommand::Resume, cx);
                }
            } else {
                // TODO: start a different episode on the active device at
                // `at_ms`; `play` there starts by the replay rule instead.
                self.command_remote(ConnectCommand::Play { episode_id: id }, cx);
            }
            return;
        }
        self.play_here(id, Some(at_ms), cx);
    }

    /// Plays `id` on this device: from `at` when given, which isn't a
    /// replay, otherwise by the start rule of spec 8. Resumes it if it is
    /// the loaded episode.
    fn play_here(&mut self, id: EpisodeId, at: Option<i64>, cx: &mut Context<Self>) {
        if let Some(current) = &self.current
            && current.episode.id == id
            && current.session().is_some()
        {
            if let Some(at) = at {
                self.seek_local(at);
            }
            self.resume_local();
            self.report(None, cx);
            cx.notify();
            return;
        }
        // An episode change: the old play's last position write first.
        self.end_session(Contract::stop);
        self.requests += 1;
        let request = self.requests;
        self.loading = Some(request);
        let loaded = self.load(Some(id));
        cx.spawn(async move |this, cx| {
            let episode = loaded.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                if this.requests != request {
                    return;
                }
                this.loading = None;
                match episode {
                    Some(mut episode) => {
                        if let Some(position_ms) = at {
                            episode.stored = Stored {
                                position_ms,
                                played: false,
                            };
                        }
                        this.start(episode, cx);
                    }
                    None => this.not_here(id, at, cx),
                }
            });
        })
        .detach();
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        match &self.current {
            Some(current) if current.is_playing() => self.pause(cx),
            Some(current) if current.session().is_some() || current.remote().is_some() => {
                self.resume(cx);
            }
            Some(current) => {
                let id = current.episode.id;
                self.play(id, cx);
            }
            None => {}
        }
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if self.is_remote() {
            self.command_remote(ConnectCommand::Pause, cx);
            return;
        }
        if self.pause_local() {
            self.report(None, cx);
            cx.notify();
        }
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        if self.is_remote() {
            self.command_remote(ConnectCommand::Resume, cx);
            return;
        }
        if self.resume_local() {
            self.report(None, cx);
            cx.notify();
        }
    }

    /// Skips by the synced skip interval.
    pub fn skip(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(current) = &self.current else {
            return;
        };
        let delta = if forward {
            self.settings.skip_forward_ms
        } else {
            -self.settings.skip_back_ms
        };
        if current.remote().is_some() {
            // The active device skips from its own position, which the bar
            // only guesses between updates.
            self.command_remote(ConnectCommand::Skip { delta_ms: delta }, cx);
            return;
        }
        self.seek(current.position_ms().saturating_add(delta), cx);
    }

    /// Moves to `target_ms`, within the episode, and saves the position.
    /// With nothing loaded it moves the parked position.
    pub fn seek(&mut self, target_ms: i64, cx: &mut Context<Self>) {
        let Some(current) = &self.current else {
            return;
        };
        if current.remote().is_some() {
            let end = current.duration_ms().unwrap_or(i64::MAX);
            let position_ms = target_ms.clamp(0, end);
            self.command_remote(ConnectCommand::Seek { position_ms }, cx);
            return;
        }
        self.seek_local(target_ms);
        self.report(None, cx);
        cx.notify();
    }

    /// Steps `playback_speed` up, wrapping around, and writes the synced
    /// setting so every device plays at it, the active one included.
    pub fn cycle_speed(&mut self, cx: &mut Context<Self>) {
        let speed = next_step(&SPEEDS, self.settings.speed);
        self.settings.speed = speed;
        self.send(Command::Speed(speed));
        self.write_setting(settings::PLAYBACK_SPEED, serde_json::json!(speed));
        self.report_speed(cx);
        cx.notify();
    }

    /// Steps a skip interval up, wrapping around, and writes the synced
    /// setting.
    pub fn cycle_skip(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (key, interval) = if forward {
            (
                settings::SKIP_FORWARD_MS,
                &mut self.settings.skip_forward_ms,
            )
        } else {
            (settings::SKIP_BACK_MS, &mut self.settings.skip_back_ms)
        };
        *interval = next_step(&SKIP_STEPS_MS, *interval);
        let value = serde_json::json!(*interval);
        self.write_setting(key, value);
        cx.notify();
    }

    /// Empties the player after sign-out: unloads mpv and drops the
    /// episode without a position write, since its account's state is gone.
    /// A play still loading from the store doesn't start.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.session_mut().is_some() {
            self.send(Command::Stop);
            self.reconciler.set_playing(None);
        }
        self.requests += 1;
        self.loading = None;
        self.current = None;
        cx.notify();
    }

    /// The last position write, before the app quits. Returns the worker,
    /// which ends once that write and everything before it has landed.
    /// Closes the event stream, so the server knows the device is gone.
    pub fn quit(&mut self) -> Option<JoinHandle<()>> {
        self.stream = None;
        self.end_session(Contract::stop);
        let _ = self.jobs.send(Job::Finish);
        self.worker.take()
    }

    // ==========================================================================
    // The local player
    // ==========================================================================

    /// Pauses mpv and saves the position. Returns whether it was playing.
    fn pause_local(&mut self) -> bool {
        let Some(session) = self.session_mut() else {
            return false;
        };
        let writes = session.pause(Instant::now());
        if writes.is_empty() {
            return false;
        }
        self.send(Command::Pause(true));
        self.issue(writes);
        true
    }

    /// Resumes the loaded play. Returns whether it was paused.
    fn resume_local(&mut self) -> bool {
        let Some(session) = self.session_mut() else {
            return false;
        };
        if session.is_playing() {
            return false;
        }
        session.resume(Instant::now());
        self.send(Command::Pause(false));
        true
    }

    /// Resumes the loaded play, or starts the episode in the player by the
    /// start rule: what play does here.
    fn resume_or_start(&mut self, cx: &mut Context<Self>) {
        match &self.current {
            Some(current) if current.session().is_some() => {
                if self.resume_local() {
                    self.report(None, cx);
                }
            }
            Some(current) => {
                let id = current.episode.id;
                self.play_here(id, None, cx);
            }
            None => {}
        }
    }

    /// Moves the local play, or the parked position, to `target_ms` within
    /// the episode, and saves it.
    fn seek_local(&mut self, target_ms: i64) {
        let Some(current) = &mut self.current else {
            return;
        };
        if current.remote().is_some() {
            return;
        }
        let end = current.duration_ms().unwrap_or(i64::MAX);
        let target = target_ms.clamp(0, end);
        let writes = match current.session_mut() {
            Some(session) => {
                // TODO: mpv can't seek before it has opened the file, so a
                // skip in the first moment of a play is lost.
                let writes = session.seek(target, Instant::now());
                self.send(Command::Seek(target));
                writes
            }
            None => {
                current.parked_ms = target;
                vec![Write::Position {
                    position_ms: target,
                    while_playing: false,
                }]
            }
        };
        self.issue(writes);
    }

    /// Whether mpv plays the episode here.
    pub(super) fn is_playing_here(&self) -> bool {
        self.current
            .as_ref()
            .and_then(Current::session)
            .is_some_and(Contract::is_playing)
    }

    /// Whether this device plays, or is about to: a play reading its
    /// episode from the store counts.
    fn is_busy_here(&self) -> bool {
        self.loading.is_some() || self.is_playing_here()
    }

    fn start(&mut self, episode: Episode, cx: &mut Context<Self>) {
        let (session, writes) = Contract::start(
            episode.stored,
            episode.duration_ms,
            self.settings.played_threshold_ms,
            Instant::now(),
        );
        self.send(Command::Load {
            source: episode.source.for_mpv(),
            start_ms: session.position_ms(),
            paused: false,
        });
        self.pending_starts += 1;
        self.reconciler.set_playing(Some(episode.id));
        let _ = self.jobs.send(Job::Remember(episode.id));
        self.current = Some(Current::new(
            episode,
            session.position_ms(),
            Phase::Loaded(session),
        ));
        self.issue(writes);
        self.report(None, cx);
        cx.notify();
    }

    /// The episode to play isn't in the store. One the account's playback
    /// names shows parked, so the server learns it isn't playing here.
    fn not_here(&mut self, id: EpisodeId, at: Option<i64>, cx: &mut Context<Self>) {
        tracing::warn!("episode {id} to play isn't in the store");
        if let Some(playback) = self
            .playback
            .clone()
            .filter(|playback| playback.episode.id == id)
        {
            let position_ms = at.unwrap_or(playback.position_ms);
            self.show_episode(&playback.episode, Phase::Parked, position_ms, false, cx);
            self.report(None, cx);
        }
        cx.emit(ConnectNotice(
            "This episode isn't on this device yet.".to_owned(),
        ));
        cx.notify();
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        if event == Event::Started {
            self.pending_starts = self.pending_starts.saturating_sub(1);
            return;
        }
        if self.pending_starts > 0 {
            return;
        }
        let now = Instant::now();
        match event {
            Event::Started => {}
            Event::Duration(duration_ms) => {
                if let Some(session) = self.session_mut() {
                    session.set_duration(duration_ms);
                    cx.notify();
                }
            }
            Event::Position(position_ms) => {
                let Some(session) = self.session_mut() else {
                    return;
                };
                // The bar shows whole seconds; mpv reports more often.
                let shown_second = session.position_ms() / 1000;
                let writes = session.progress(position_ms, now);
                if position_ms / 1000 != shown_second {
                    cx.notify();
                }
                self.issue(writes);
            }
            Event::Ended => {
                if self.session_mut().is_none() {
                    return;
                }
                self.end_session(|session, now| session.ended(now));
                self.report(None, cx);
                cx.notify();
            }
            Event::Failed(failure) => {
                tracing::error!(?failure, "playback failed");
                if self.session_mut().is_none() {
                    return;
                }
                self.end_session(Contract::stop);
                if let Some(current) = &mut self.current {
                    current.phase = Phase::Failed(failure);
                }
                self.report(None, cx);
                cx.notify();
            }
        }
    }

    /// Ends the play in progress with `end`'s writes and parks the episode
    /// where it stopped. Releases the file to the reconciler's grace period.
    fn end_session(&mut self, end: impl FnOnce(Contract, Instant) -> Vec<Write>) {
        let Some(current) = &mut self.current else {
            return;
        };
        let session = match std::mem::replace(&mut current.phase, Phase::Parked) {
            Phase::Loaded(session) => session,
            // Another device's play isn't the local player's to end.
            remote @ Phase::Remote { .. } => {
                current.phase = remote;
                return;
            }
            Phase::Parked | Phase::Failed(_) => return,
        };
        if let Some(duration_ms) = session.duration_ms() {
            current.episode.duration_ms = Some(duration_ms);
        }
        current.parked_ms = session.position_ms();
        let writes = end(session, Instant::now());
        // Every ending saves the position it ended at.
        if let Some(Write::Position { position_ms, .. }) = writes.last() {
            current.parked_ms = *position_ms;
        }
        self.reconciler.set_playing(None);
        self.issue(writes);
    }

    /// A sync brought new settings. A new speed reaches mpv, and the
    /// server through a report while the episode plays here.
    fn apply_settings(&mut self, settings: PlayerSettings, cx: &mut Context<Self>) {
        let speed_changed = settings.speed != self.settings.speed;
        self.settings = settings;
        if speed_changed {
            self.send(Command::Speed(settings.speed));
            self.report_speed(cx);
        }
    }

    /// Reports a speed change of the loaded play.
    fn report_speed(&mut self, cx: &mut Context<Self>) {
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.session().is_some())
        {
            self.report(None, cx);
        }
    }

    fn session_mut(&mut self) -> Option<&mut Contract> {
        self.current.as_mut()?.session_mut()
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::error!("the mpv command thread is gone");
        }
    }

    fn load(&self, id: Option<EpisodeId>) -> oneshot::Receiver<Option<Episode>> {
        let (reply, receiver) = oneshot::channel();
        let _ = self.jobs.send(Job::Load { id, reply });
        receiver
    }

    /// Hands the contract's writes for the current episode to the worker.
    fn issue(&self, writes: Vec<Write>) {
        let Some(current) = &self.current else {
            return;
        };
        for write in writes {
            let change = match write {
                Write::Position {
                    position_ms,
                    while_playing,
                } => EpisodeChange::Position {
                    position_ms,
                    while_playing,
                },
                Write::Played(played) => EpisodeChange::Played(played),
            };
            let _ = self.jobs.send(Job::Write(LocalChange::Episode {
                episode_id: current.episode.id,
                podcast_id: current.episode.podcast_id,
                change,
            }));
        }
    }

    fn write_setting(&self, key: &str, value: serde_json::Value) {
        let _ = self.jobs.send(Job::Write(LocalChange::Setting {
            key: key.to_owned(),
            value,
        }));
    }
}

/// Creates the player and registers it as the [`PlayerHandle`] global.
pub fn install(engine: mpv::Engine, cx: &mut App) -> Entity<Player> {
    let player = cx.new(|cx| Player::new(engine, cx));
    cx.set_global(PlayerHandle(player.clone()));
    player
}
