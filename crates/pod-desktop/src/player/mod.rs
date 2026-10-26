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

pub mod contract;
pub mod mpv;
pub mod settings;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, Task};
use pod_protocol::{EpisodeId, PodcastId};
use rusqlite::{Connection, OptionalExtension as _};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use self::contract::{Contract, Stored, Write};
use self::mpv::{Command, Event};
use self::settings::{PlayerSettings, SKIP_STEPS_MS, SPEEDS, next_step};
use crate::reconcile::Reconciler;
use crate::runtime::Background;
use crate::store::{Store, Table, live};
use crate::sync::SyncHandle;
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
                    coalesce(s.played, 0), d.path
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
                ))
            },
        )
        .optional()
        .context("query the episode")?;
    let Some((podcast_id, title, podcast_title, duration_ms, audio_url, position_ms, played, path)) =
        row
    else {
        return Ok(None);
    };
    Ok(Some(Episode {
        id,
        podcast_id: podcast_id.parse().context("parse the podcast id")?,
        title,
        podcast_title,
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
    /// start, or after it ended or failed.
    parked_ms: i64,
    /// The play in progress, while mpv has the file loaded (playing or
    /// paused).
    session: Option<Contract>,
}

impl Current {
    pub fn position_ms(&self) -> i64 {
        self.session
            .as_ref()
            .map_or(self.parked_ms, Contract::position_ms)
    }

    pub fn duration_ms(&self) -> Option<i64> {
        self.session
            .as_ref()
            .and_then(Contract::duration_ms)
            .or(self.episode.duration_ms)
    }

    pub fn is_playing(&self) -> bool {
        self.session.as_ref().is_some_and(Contract::is_playing)
    }
}

pub struct Player {
    commands: mpsc::UnboundedSender<Command>,
    jobs: mpsc::UnboundedSender<Job>,
    worker: Option<JoinHandle<()>>,
    reconciler: Reconciler,
    settings: PlayerSettings,
    current: Option<Current>,
    /// Loads sent to mpv whose start it hasn't reported. Until it has,
    /// mpv's events are about the file being replaced.
    pending_starts: usize,
    /// Counts play requests, so only the newest one's load starts playing.
    requests: u64,
    _events: Task<()>,
    _settings: Task<()>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("settings", &self.settings)
            .field("current", &self.current)
            .field("pending_starts", &self.pending_starts)
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Requires the [`Store`], [`SyncHandle`], [`Background`] and
    /// [`Reconciler`] globals. Restores the remembered episode, paused.
    pub fn new(engine: mpv::Engine, cx: &mut Context<Self>) -> Self {
        let reconciler = cx.global::<Reconciler>().clone();
        let (jobs, job_receiver) = mpsc::unbounded_channel();
        let worker = cx.global::<Background>().spawn(work(
            cx.global::<Store>().clone(),
            cx.global::<SyncHandle>().clone(),
            reconciler.root().to_owned(),
            job_receiver,
        ));

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
            settings: PlayerSettings::default(),
            current: None,
            pending_starts: 0,
            requests: 0,
            _events: events,
            _settings: live::observe_tables(
                cx,
                Table::Setting.into(),
                PlayerSettings::load,
                |this: &mut Self, settings, _| this.apply_settings(settings),
            ),
        };

        let restored = player.load(None);
        cx.spawn(async move |this, cx| {
            let Ok(Some(episode)) = restored.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                // A play that started meanwhile wins over the restore.
                if this.current.is_none() && this.requests == 0 {
                    this.current = Some(Current {
                        parked_ms: episode.stored.start_position_ms(),
                        episode,
                        session: None,
                    });
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
    /// loaded episode.
    pub fn play(&mut self, id: EpisodeId, cx: &mut Context<Self>) {
        if let Some(current) = &self.current
            && current.episode.id == id
            && current.session.is_some()
        {
            self.resume(cx);
            return;
        }
        // An episode change: the old play's last position write first.
        self.end_session(Contract::stop);
        self.requests += 1;
        let request = self.requests;
        let loaded = self.load(Some(id));
        cx.spawn(async move |this, cx| {
            let Ok(Some(episode)) = loaded.await else {
                tracing::warn!("episode {id} to play isn't in the store");
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.requests == request {
                    this.start(episode, cx);
                }
            });
        })
        .detach();
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        match &self.current {
            Some(current) if current.is_playing() => self.pause(cx),
            Some(current) if current.session.is_some() => self.resume(cx),
            Some(current) => {
                let id = current.episode.id;
                self.play(id, cx);
            }
            None => {}
        }
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session_mut() else {
            return;
        };
        let writes = session.pause(Instant::now());
        self.send(Command::Pause(true));
        self.issue(writes);
        cx.notify();
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session_mut() else {
            return;
        };
        session.resume(Instant::now());
        self.send(Command::Pause(false));
        cx.notify();
    }

    /// Skips by the synced skip interval. A seek, so it saves the position;
    /// with nothing loaded it moves the parked position.
    pub fn skip(&mut self, forward: bool, cx: &mut Context<Self>) {
        let settings = self.settings;
        let Some(current) = &mut self.current else {
            return;
        };
        let delta = if forward {
            settings.skip_forward_ms
        } else {
            -settings.skip_back_ms
        };
        let end = current.duration_ms().unwrap_or(i64::MAX);
        let target = current.position_ms().saturating_add(delta).clamp(0, end);
        let writes = match &mut current.session {
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
        cx.notify();
    }

    /// Steps `playback_speed` up, wrapping around, and writes the synced
    /// setting so every device plays at it.
    pub fn cycle_speed(&mut self, cx: &mut Context<Self>) {
        let speed = next_step(&SPEEDS, self.settings.speed);
        self.settings.speed = speed;
        self.send(Command::Speed(speed));
        self.write_setting(settings::PLAYBACK_SPEED, serde_json::json!(speed));
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

    /// The last position write, before the app quits. Returns the worker,
    /// which ends once that write and everything before it has landed.
    pub fn quit(&mut self) -> Option<JoinHandle<()>> {
        self.end_session(Contract::stop);
        let _ = self.jobs.send(Job::Finish);
        self.worker.take()
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
        self.current = Some(Current {
            parked_ms: session.position_ms(),
            episode,
            session: Some(session),
        });
        self.issue(writes);
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
                self.end_session(|session, now| session.ended(now));
                cx.notify();
            }
            Event::Failed(message) => {
                // TODO: show the failure in the player bar.
                tracing::error!("playback failed: {message}");
                self.end_session(Contract::stop);
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
        let Some(session) = current.session.take() else {
            return;
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

    fn apply_settings(&mut self, settings: PlayerSettings) {
        if settings.speed != self.settings.speed {
            self.send(Command::Speed(settings.speed));
        }
        self.settings = settings;
    }

    fn session_mut(&mut self) -> Option<&mut Contract> {
        self.current.as_mut()?.session.as_mut()
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
