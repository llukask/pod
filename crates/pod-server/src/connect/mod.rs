//! Connect (spec 14): each user's playback, the devices that hold an event
//! stream open, and the operations behind `/v1/connect/*`.
//!
//! Everything lives in memory, which assumes one server process like the
//! auth rate limiter does. A restart loses the devices, which reconnect,
//! and the playback, which [`Connect::ensure_loaded`] rebuilds from the
//! Sonos sessions and the newest position write.
//!
//! Locking: `accounts` is a plain mutex held only for map and channel
//! operations, never across an await. A transfer awaits the source's
//! release and the speakers, so transfers of one user queue on that user's
//! async `transfers` lock instead, while reports, commands and Sonos changes
//! keep flowing (a transfer waits for a report).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use color_eyre::eyre::{Report, Result, WrapErr};
use pod_protocol::{
    ConnectCommand, ConnectDevice, ConnectEvent, ConnectReport, ConnectState, DeviceId, DeviceKind,
    EpisodeId, Playback, PlaybackEpisode, PodcastId, SonosCommand, SonosPlayRequest, SonosPlayer,
    Stamp, UserId,
};
use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use uuid::Uuid;

use crate::clock::Clock;
use crate::sonos::{Sonos, SonosChange, SonosError};
use crate::sync::push::{EpisodeWrite, write_episode_state};

/// How long the active device may stay disconnected before the playback
/// stops naming it (spec 14.1).
const GRACE: Duration = Duration::from_secs(15);

/// How long a transfer waits for the source's release report (spec 14.5).
const RELEASE_WAIT: Duration = Duration::from_secs(3);

/// Events queued per stream. A client that falls this far behind is cut
/// off and reconnects to a fresh `state`.
const STREAM_BUFFER: usize = 64;

/// Why a Connect operation failed, by the response it becomes.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// `404 not_found`: no readable episode, or no such target.
    #[error("not found")]
    NotFound,
    /// `409 no_session`: no playback, no active device, or this device has
    /// no stream.
    #[error("no session")]
    NoSession,
    /// `400 bad_request`.
    #[error("{0}")]
    BadRequest(&'static str),
    /// A Sonos operation failed.
    #[error(transparent)]
    Sonos(SonosError),
    /// `500 internal`.
    #[error("{0:#}")]
    Internal(Report),
}

impl From<Report> for ConnectError {
    fn from(report: Report) -> Self {
        Self::Internal(report)
    }
}

impl From<SonosError> for ConnectError {
    fn from(error: SonosError) -> Self {
        match error {
            SonosError::Internal(report) => Self::Internal(report),
            other => Self::Sonos(other),
        }
    }
}

/// How a command reached the active device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// A Sonos group ran it; the playback shows the result (`200`).
    Done,
    /// A client device got it over its stream; its next report shows the
    /// result (`202`).
    Sent,
}

// ==============================================================================
// State
// ==============================================================================

#[derive(Debug, Default)]
struct Account {
    /// Whether the playback was rebuilt from the database since the server
    /// started (or something newer replaced it first).
    loaded: bool,
    /// Connected client devices, in connection order.
    devices: Vec<Device>,
    playback: Option<Current>,
    /// Transfers waiting for their source's release report.
    releases: HashMap<Uuid, oneshot::Sender<ConnectReport>>,
    /// Bumped whenever the active device changes, so a grace timer started
    /// for an earlier one does nothing.
    generation: u64,
    transfers: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug)]
struct Device {
    info: ConnectDevice,
    stream: u64,
    events: mpsc::Sender<ConnectEvent>,
}

/// The playback with the moment its position was observed.
#[derive(Debug, Clone)]
struct Current {
    active: Option<Active>,
    episode: PlaybackEpisode,
    position_ms: i64,
    playing: bool,
    speed: f64,
    volume: Option<u8>,
    at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Active {
    Client(DeviceId),
    Sonos {
        player_id: String,
        /// The Sonos session's device id once a change named it.
        session: Option<DeviceId>,
    },
}

impl Active {
    fn id(&self) -> String {
        match self {
            Active::Client(device) => device.to_string(),
            Active::Sonos { player_id, .. } => player_id.clone(),
        }
    }

    fn is_sonos(&self, player: &str) -> bool {
        matches!(self, Active::Sonos { player_id, .. } if player_id == player)
    }
}

impl Current {
    /// The position at `now`: while playing, the observed one advanced by
    /// the time since, at `speed`.
    fn position_at(&self, now: Instant) -> i64 {
        if !self.playing {
            return self.position_ms;
        }
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64() * 1000.0;
        // Truncating sub-millisecond progress is fine.
        #[allow(clippy::cast_possible_truncation)]
        let advanced = (elapsed * self.speed) as i64;
        self.position_ms.saturating_add(advanced)
    }

    fn view(&self, now: Instant) -> Playback {
        Playback {
            device_id: self.active.as_ref().map(Active::id),
            episode: self.episode.clone(),
            position_ms: self.position_at(now),
            playing: self.playing,
            speed: self.speed,
            volume: self.volume,
        }
    }
}

impl Account {
    fn device(&self, id: DeviceId) -> Option<&Device> {
        self.devices.iter().find(|device| device.info.id == id)
    }

    fn state(&self, now: Instant) -> ConnectState {
        ConnectState {
            devices: self
                .devices
                .iter()
                .map(|device| device.info.clone())
                .collect(),
            playback: self.playback.as_ref().map(|current| current.view(now)),
        }
    }

    fn view(&self, now: Instant) -> Option<Playback> {
        self.playback.as_ref().map(|current| current.view(now))
    }

    fn active(&self) -> Option<&Active> {
        self.playback.as_ref()?.active.as_ref()
    }

    /// Sends `event` to one device. `false` when it isn't connected, or
    /// fell so far behind that it was cut off.
    fn send(&mut self, device: DeviceId, event: ConnectEvent) -> bool {
        let Some(index) = self.devices.iter().position(|d| d.info.id == device) else {
            return false;
        };
        if self.devices[index].events.try_send(event).is_ok() {
            return true;
        }
        // Dropping the sender ends the stream; the client reconnects.
        self.devices.remove(index);
        false
    }
}

/// What a user's devices must hear after a change, computed under the lock
/// and acted on after it, since acting may spawn tasks.
#[derive(Debug, Default)]
#[must_use]
struct Effects {
    /// Sonos sessions to release because another device claimed the
    /// playback.
    release_sonos: Vec<String>,
    /// Devices that dropped out while active, which start the grace timer.
    gone: Vec<(DeviceId, u64)>,
}

// ==============================================================================
// The hub
// ==============================================================================

#[derive(Debug)]
pub struct Connect {
    pool: PgPool,
    sonos: Option<Arc<Sonos>>,
    clock: Clock,
    accounts: Mutex<HashMap<UserId, Account>>,
    next_stream: AtomicU64,
    grace: Duration,
    release_wait: Duration,
}

/// Detaches a stream's device when the stream ends, however it ends.
#[derive(Debug)]
pub struct StreamGuard {
    connect: Arc<Connect>,
    user_id: UserId,
    device: DeviceId,
    stream: u64,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.connect.detach(self.user_id, self.device, self.stream);
    }
}

impl Connect {
    /// The hub. With Sonos on, it follows the Sonos sessions from a task
    /// that runs as long as the hub does, so this needs a Tokio runtime.
    pub fn new(pool: PgPool, sonos: Option<Arc<Sonos>>, clock: Clock) -> Arc<Self> {
        Self::with_timing(pool, sonos, clock, GRACE, RELEASE_WAIT)
    }

    /// [`Connect::new`] with other grace and release timeouts, for tests.
    pub fn with_timing(
        pool: PgPool,
        sonos: Option<Arc<Sonos>>,
        clock: Clock,
        grace: Duration,
        release_wait: Duration,
    ) -> Arc<Self> {
        let changes = sonos.as_ref().map(|sonos| sonos.subscribe());
        let connect = Arc::new(Self {
            pool,
            sonos,
            clock,
            accounts: Mutex::new(HashMap::new()),
            next_stream: AtomicU64::new(1),
            grace,
            release_wait,
        });
        if let Some(changes) = changes {
            // A weak handle, so the task ends with the hub instead of
            // keeping it alive.
            tokio::spawn(follow_sonos(Arc::downgrade(&connect), changes));
        }
        connect
    }

    fn accounts(&self) -> MutexGuard<'_, HashMap<UserId, Account>> {
        // Nothing panics while holding the lock (map and channel
        // operations), and the data stays valid either way.
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // ==========================================================================
    // Streams
    // ==========================================================================

    /// Connects a device: it becomes available, every stream of the user
    /// gets the new `state`, and an earlier stream of the same device is
    /// replaced (spec 14.2). The stream ends when the receiver's sender is
    /// dropped; dropping the guard detaches the device.
    pub async fn attach(
        self: &Arc<Self>,
        user_id: UserId,
        device: DeviceId,
        kind: DeviceKind,
    ) -> Result<(mpsc::Receiver<ConnectEvent>, StreamGuard), ConnectError> {
        self.ensure_loaded(user_id).await?;
        let name = sqlx::query_scalar!(
            "select device_name from session where device_id = $1",
            device.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await
        .context("query the device name")?
        // Logged out since the request authenticated.
        .ok_or(ConnectError::NoSession)?;
        let stream = self.next_stream.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel(STREAM_BUFFER);
        {
            let mut accounts = self.accounts();
            let account = accounts.entry(user_id).or_default();
            if let Some(index) = account.devices.iter().position(|d| d.info.id == device) {
                let replaced = account.devices.remove(index);
                let _ = replaced.events.try_send(ConnectEvent::Replaced);
            }
            account.devices.push(Device {
                info: ConnectDevice {
                    id: device,
                    name,
                    kind,
                },
                stream,
                events: sender,
            });
            if account.active() == Some(&Active::Client(device)) {
                // Back within the grace period: its earlier timer must not
                // fire on a later disconnect's behalf.
                account.generation += 1;
            }
            let effects = Self::broadcast(account);
            drop(accounts);
            self.apply(user_id, effects);
        }
        tracing::debug!(%user_id, %device, ?kind, "Connect device attached");
        Ok((
            receiver,
            StreamGuard {
                connect: Arc::clone(self),
                user_id,
                device,
                stream,
            },
        ))
    }

    fn detach(self: &Arc<Self>, user_id: UserId, device: DeviceId, stream: u64) {
        let mut accounts = self.accounts();
        let Some(account) = accounts.get_mut(&user_id) else {
            return;
        };
        let before = account.devices.len();
        account
            .devices
            .retain(|d| !(d.info.id == device && d.stream == stream));
        if account.devices.len() != before {
            tracing::debug!(%user_id, %device, "Connect device detached");
            let effects = Self::broadcast(account);
            drop(accounts);
            self.apply(user_id, effects);
        }
    }

    /// Sends the current `state` to every stream of the user. Streams that
    /// can't take it are cut off, and an active device among them starts
    /// the grace timer, as does one that detached.
    fn broadcast(account: &mut Account) -> Effects {
        let now = Instant::now();
        let event = ConnectEvent::State(account.state(now));
        account
            .devices
            .retain(|device| device.events.try_send(event.clone()).is_ok());
        let mut effects = Effects::default();
        if let Some(Active::Client(active)) = account.active()
            && account.device(*active).is_none()
        {
            effects.gone.push((*active, account.generation));
        }
        effects
    }

    /// Acts on what a change left to do, outside the accounts lock.
    fn apply(self: &Arc<Self>, user_id: UserId, effects: Effects) {
        for player_id in effects.release_sonos {
            let Some(sonos) = self.sonos.clone() else {
                continue;
            };
            let (pool, now) = (self.pool.clone(), self.clock.now());
            tokio::spawn(async move {
                match sonos.release(&pool, user_id, &player_id, now).await {
                    Ok(_) | Err(SonosError::NoSession | SonosError::NotFound) => {}
                    Err(error) => tracing::warn!(
                        %user_id, player_id, error = %error,
                        "releasing a Sonos session another device took over failed"
                    ),
                }
            });
        }
        for (device, generation) in effects.gone {
            let connect = Arc::clone(self);
            tokio::spawn(async move {
                tokio::time::sleep(connect.grace).await;
                connect.expire(user_id, device, generation);
            });
        }
    }

    /// The grace timer of spec 14.1, "Device gone": the playback stops
    /// naming a device that stayed away, paused at its last report.
    fn expire(self: &Arc<Self>, user_id: UserId, device: DeviceId, generation: u64) {
        let mut accounts = self.accounts();
        let Some(account) = accounts.get_mut(&user_id) else {
            return;
        };
        if account.generation != generation || account.device(device).is_some() {
            return;
        }
        let Some(current) = account.playback.as_mut() else {
            return;
        };
        if current.active != Some(Active::Client(device)) {
            return;
        }
        current.active = None;
        current.playing = false;
        current.at = Instant::now();
        account.generation += 1;
        tracing::debug!(%user_id, %device, "the active Connect device went away");
        let effects = Self::broadcast(account);
        drop(accounts);
        self.apply(user_id, effects);
    }

    /// Makes `active` the active device and releases the previous one when
    /// that is another device (spec 14.1, "Claim").
    fn claim(account: &mut Account, active: Active, effects: &mut Effects) {
        let previous = account.active().cloned();
        let same = match (&previous, &active) {
            (Some(Active::Client(a)), Active::Client(b)) => a == b,
            (Some(Active::Sonos { player_id: a, .. }), Active::Sonos { player_id: b, .. }) => {
                a == b
            }
            _ => false,
        };
        if !same {
            account.generation += 1;
            match previous {
                Some(Active::Client(device)) => {
                    account.send(device, ConnectEvent::Release { transfer_id: None });
                }
                Some(Active::Sonos { player_id, .. }) => effects.release_sonos.push(player_id),
                None => {}
            }
        }
        if let Some(current) = account.playback.as_mut() {
            current.active = Some(active);
        }
    }

    // ==========================================================================
    // Reads
    // ==========================================================================

    /// The devices and the playback (spec 14.6).
    pub async fn state(&self, user_id: UserId) -> Result<ConnectState, ConnectError> {
        self.ensure_loaded(user_id).await?;
        let accounts = self.accounts();
        Ok(accounts
            .get(&user_id)
            .map(|account| account.state(Instant::now()))
            .unwrap_or(ConnectState {
                devices: Vec::new(),
                playback: None,
            }))
    }

    /// Rebuilds the playback after a restart (spec 14.1): the user's Sonos
    /// session if there is one, else the newest position write, paused.
    async fn ensure_loaded(&self, user_id: UserId) -> Result<(), ConnectError> {
        if self.accounts().get(&user_id).is_some_and(|a| a.loaded) {
            return Ok(());
        }
        let session = sqlx::query!(
            "select player_id, device_id, episode_id, position_ms, playing
             from sonos_session where user_id = $1
             order by started_at desc limit 1",
            user_id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await
        .context("query the user's Sonos session")?;
        let (active, episode_id, position_ms, playing) = match session {
            Some(row) => (
                Some(Active::Sonos {
                    player_id: row.player_id,
                    session: Some(DeviceId::from_uuid(row.device_id)),
                }),
                EpisodeId::from_uuid(row.episode_id),
                row.position_ms,
                row.playing,
            ),
            None => {
                let last = sqlx::query!(
                    "select episode_id, position_ms from episode_state
                     where user_id = $1 and position_at is not null
                     order by position_at desc limit 1",
                    user_id.as_uuid(),
                )
                .fetch_optional(&self.pool)
                .await
                .context("query the last position write")?;
                match last {
                    Some(row) => (
                        None,
                        EpisodeId::from_uuid(row.episode_id),
                        row.position_ms,
                        false,
                    ),
                    None => {
                        self.accounts().entry(user_id).or_default().loaded = true;
                        return Ok(());
                    }
                }
            }
        };
        let episode = self.episode(user_id, episode_id).await?;
        let mut accounts = self.accounts();
        let account = accounts.entry(user_id).or_default();
        if !account.loaded {
            account.loaded = true;
            // Something newer (a report, a Sonos change) may have arrived
            // while this queried.
            if account.playback.is_none()
                && let Some(episode) = episode
            {
                account.playback = Some(Current {
                    active,
                    episode,
                    position_ms,
                    playing,
                    speed: 1.0,
                    volume: None,
                    at: Instant::now(),
                });
            }
        }
        Ok(())
    }

    /// The episode's metadata if the user may read it (spec 4.5).
    async fn episode(
        &self,
        user_id: UserId,
        episode_id: EpisodeId,
    ) -> Result<Option<PlaybackEpisode>> {
        let row = sqlx::query!(
            r#"select e.podcast_id, e.title, e.duration_ms,
                      coalesce(e.artwork_url, p.artwork_url) as artwork_url,
                      p.title as podcast_title
               from episode e
               join podcast p on p.id = e.podcast_id
               join subscription s on s.podcast_id = p.id
               where e.id = $2 and not e.deleted
                 and s.user_id = $1 and s.subscribed and s.replaced_by is null"#,
            user_id.as_uuid(),
            episode_id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await
        .context("query the episode")?;
        Ok(row.map(|row| PlaybackEpisode {
            id: episode_id,
            podcast_id: PodcastId::from_uuid(row.podcast_id),
            title: row.title,
            podcast_title: row.podcast_title,
            artwork_url: row.artwork_url,
            duration_ms: row.duration_ms,
        }))
    }

    /// The playback's episode when it is `episode_id`, else a fresh lookup:
    /// `None` when the user can't read it.
    async fn episode_for(
        &self,
        user_id: UserId,
        episode_id: EpisodeId,
    ) -> Result<Option<PlaybackEpisode>> {
        let known = self
            .accounts()
            .get(&user_id)
            .and_then(|account| account.playback.as_ref())
            .filter(|current| current.episode.id == episode_id)
            .map(|current| current.episode.clone());
        match known {
            Some(episode) => Ok(Some(episode)),
            None => self.episode(user_id, episode_id).await,
        }
    }

    // ==========================================================================
    // Reports
    // ==========================================================================

    /// A device's report of its local player (spec 14.3).
    pub async fn report(
        self: &Arc<Self>,
        user_id: UserId,
        device: DeviceId,
        report: ConnectReport,
    ) -> Result<Playback, ConnectError> {
        self.ensure_loaded(user_id).await?;
        let connected = self
            .accounts()
            .get(&user_id)
            .is_some_and(|account| account.device(device).is_some());
        if !connected {
            return Err(ConnectError::NoSession);
        }
        let episode = self
            .episode_for(user_id, report.episode_id)
            .await?
            .ok_or(ConnectError::NotFound)?;

        let now = Instant::now();
        let mut effects = Effects::default();
        let mut accounts = self.accounts();
        let account = accounts.entry(user_id).or_default();
        if let Some(waiting) = report
            .released
            .and_then(|transfer| account.releases.remove(&transfer))
        {
            // The transfer that asked takes it from here.
            let _ = waiting.send(report);
        } else {
            let active = account.active().cloned();
            let here = active == Some(Active::Client(device));
            if here || report.playing || active.is_none() {
                let current = Current {
                    active: active.clone(),
                    episode,
                    position_ms: report.position_ms,
                    playing: report.playing,
                    speed: report.speed,
                    volume: None,
                    at: now,
                };
                account.playback = Some(current);
                if !here {
                    Self::claim(account, Active::Client(device), &mut effects);
                }
                let gone = Self::broadcast(account);
                effects.gone.extend(gone.gone);
            }
        }
        let view = account.view(now).ok_or(ConnectError::NoSession)?;
        drop(accounts);
        self.apply(user_id, effects);
        Ok(view)
    }

    // ==========================================================================
    // Commands
    // ==========================================================================

    /// Runs a command on the active device (spec 14.4).
    pub async fn command(
        self: &Arc<Self>,
        user_id: UserId,
        command: ConnectCommand,
    ) -> Result<(Delivery, Playback), ConnectError> {
        self.ensure_loaded(user_id).await?;
        let active = self
            .accounts()
            .get(&user_id)
            .and_then(|account| account.active().cloned())
            .ok_or(ConnectError::NoSession)?;
        if let ConnectCommand::Play { episode_id } = command {
            self.episode_for(user_id, episode_id)
                .await?
                .ok_or(ConnectError::NotFound)?;
        }
        match active {
            Active::Client(device) => {
                if matches!(command, ConnectCommand::Volume { .. }) {
                    return Err(ConnectError::BadRequest(
                        "volume applies to Sonos groups only",
                    ));
                }
                let mut accounts = self.accounts();
                let account = accounts.entry(user_id).or_default();
                if !account.send(device, ConnectEvent::Command { command }) {
                    return Err(ConnectError::NoSession);
                }
                let view = account
                    .view(Instant::now())
                    .ok_or(ConnectError::NoSession)?;
                Ok((Delivery::Sent, view))
            }
            Active::Sonos { player_id, .. } => {
                let sonos = self.sonos.as_ref().ok_or(ConnectError::NoSession)?;
                let command = match command {
                    ConnectCommand::Play { episode_id } => {
                        let view = self
                            .start_sonos(user_id, &player_id, episode_id, None)
                            .await?;
                        return Ok((Delivery::Done, view));
                    }
                    ConnectCommand::Pause => SonosCommand::Pause,
                    ConnectCommand::Resume => SonosCommand::Resume,
                    ConnectCommand::Seek { position_ms } => SonosCommand::Seek { position_ms },
                    ConnectCommand::Skip { delta_ms } => SonosCommand::Skip { delta_ms },
                    ConnectCommand::Volume { volume } => SonosCommand::Volume { volume },
                };
                let player = sonos
                    .command(&self.pool, user_id, &player_id, command, self.clock.now())
                    .await
                    .map_err(|error| match error {
                        // The session ended (the change says so too) or the
                        // group is gone.
                        SonosError::NoSession | SonosError::NotFound => ConnectError::NoSession,
                        other => other.into(),
                    })?;
                let view = self.sonos_answered(user_id, &player)?;
                Ok((Delivery::Done, view))
            }
        }
    }

    /// Applies a Sonos group's answer to a command to the playback.
    fn sonos_answered(
        self: &Arc<Self>,
        user_id: UserId,
        player: &SonosPlayer,
    ) -> Result<Playback, ConnectError> {
        let now = Instant::now();
        let mut accounts = self.accounts();
        let account = accounts.entry(user_id).or_default();
        if let Some(current) = account.playback.as_mut()
            && current
                .active
                .as_ref()
                .is_some_and(|a| a.is_sonos(&player.id))
        {
            current.volume = Some(player.volume);
            if let Some(session) = player.session {
                current.position_ms = session.position_ms;
                current.playing = session.playing;
                current.at = now;
            }
        }
        let effects = Self::broadcast(account);
        let view = account.view(now).ok_or(ConnectError::NoSession)?;
        drop(accounts);
        self.apply(user_id, effects);
        Ok(view)
    }

    // ==========================================================================
    // Transfer
    // ==========================================================================

    /// Moves the playback to `target` and plays it there (spec 14.5).
    pub async fn transfer(
        self: &Arc<Self>,
        user_id: UserId,
        target: &str,
    ) -> Result<Playback, ConnectError> {
        self.ensure_loaded(user_id).await?;
        let transfers = Arc::clone(&self.accounts().entry(user_id).or_default().transfers);
        let _queued = transfers.lock().await;

        let target = self.target(user_id, target).await?;
        let current = self
            .accounts()
            .get(&user_id)
            .and_then(|account| account.playback.clone())
            .ok_or(ConnectError::NoSession)?;
        let already = match (&current.active, &target) {
            (Some(Active::Client(a)), Active::Client(b)) => a == b,
            (Some(active), Active::Sonos { player_id, .. }) => active.is_sonos(player_id),
            _ => false,
        };
        if already {
            if current.playing {
                return Ok(current.view(Instant::now()));
            }
            return self
                .command(user_id, ConnectCommand::Resume)
                .await
                .map(|(_, view)| view);
        }

        // 1. Release the source.
        let (episode_id, position_ms) = match &current.active {
            Some(Active::Client(source)) => self.release_client(user_id, *source, &current).await,
            Some(Active::Sonos { player_id, .. }) => {
                let sonos = self.sonos.as_ref().ok_or(ConnectError::NoSession)?;
                match sonos
                    .release(&self.pool, user_id, player_id, self.clock.now())
                    .await
                {
                    Ok(released) => (released.episode_id, Some(released.position_ms)),
                    // Already ended: its last position is written.
                    Err(SonosError::NoSession | SonosError::NotFound) => (
                        current.episode.id,
                        Some(current.position_at(Instant::now())),
                    ),
                    Err(error) => return Err(error.into()),
                }
            }
            None => (current.episode.id, None),
        };
        let episode = self
            .episode_for(user_id, episode_id)
            .await?
            .ok_or(ConnectError::NotFound)?;
        {
            let now = Instant::now();
            let mut accounts = self.accounts();
            let account = accounts.entry(user_id).or_default();
            account.generation += 1;
            account.playback = Some(Current {
                active: None,
                episode: episode.clone(),
                position_ms: position_ms.unwrap_or(current.position_ms),
                playing: false,
                speed: current.speed,
                volume: None,
                at: now,
            });
            let effects = Self::broadcast(account);
            drop(accounts);
            self.apply(user_id, effects);
        }

        // 2. Start the target.
        match target {
            Active::Client(device) => {
                let now = Instant::now();
                let mut accounts = self.accounts();
                let account = accounts.entry(user_id).or_default();
                let load = ConnectEvent::Load {
                    episode_id,
                    position_ms,
                };
                if !account.send(device, load) {
                    return Err(ConnectError::NotFound);
                }
                let mut effects = Effects::default();
                Self::claim(account, Active::Client(device), &mut effects);
                let gone = Self::broadcast(account);
                effects.gone.extend(gone.gone);
                let view = account.view(now).ok_or(ConnectError::NoSession)?;
                drop(accounts);
                self.apply(user_id, effects);
                Ok(view)
            }
            Active::Sonos { player_id, .. } => {
                self.start_sonos(user_id, &player_id, episode_id, position_ms)
                    .await
            }
        }
    }

    /// A transfer's target: a connected client device of the user, or a
    /// current Sonos player.
    async fn target(&self, user_id: UserId, target: &str) -> Result<Active, ConnectError> {
        if let Ok(device) = target.parse::<DeviceId>() {
            let connected = self
                .accounts()
                .get(&user_id)
                .is_some_and(|account| account.device(device).is_some());
            return if connected {
                Ok(Active::Client(device))
            } else {
                Err(ConnectError::NotFound)
            };
        }
        let sonos = self.sonos.as_ref().ok_or(ConnectError::NotFound)?;
        if sonos.has_player(target).await? {
            Ok(Active::Sonos {
                player_id: target.to_owned(),
                session: None,
            })
        } else {
            Err(ConnectError::NotFound)
        }
    }

    /// Asks a client source to release and waits for its report, or takes
    /// the last reported position, advanced if it was playing.
    async fn release_client(
        &self,
        user_id: UserId,
        source: DeviceId,
        current: &Current,
    ) -> (EpisodeId, Option<i64>) {
        let transfer = Uuid::now_v7();
        let (sender, receiver) = oneshot::channel();
        let sent = {
            let mut accounts = self.accounts();
            let account = accounts.entry(user_id).or_default();
            account.releases.insert(transfer, sender);
            account.send(
                source,
                ConnectEvent::Release {
                    transfer_id: Some(transfer),
                },
            )
        };
        let released = if sent {
            tokio::time::timeout(self.release_wait, receiver).await.ok()
        } else {
            None
        };
        if let Some(account) = self.accounts().get_mut(&user_id) {
            account.releases.remove(&transfer);
        }
        match released {
            Some(Ok(report)) => (report.episode_id, Some(report.position_ms)),
            _ => {
                tracing::debug!(%user_id, %source, "a transfer's source didn't report its release");
                (
                    current.episode.id,
                    Some(current.position_at(Instant::now())),
                )
            }
        }
    }

    /// Starts the episode on a Sonos group at `position_ms`, or by the start
    /// rule of spec 8 over the server's state when that is `None`.
    async fn start_sonos(
        self: &Arc<Self>,
        user_id: UserId,
        player_id: &str,
        episode_id: EpisodeId,
        position_ms: Option<i64>,
    ) -> Result<Playback, ConnectError> {
        let sonos = self.sonos.as_ref().ok_or(ConnectError::NotFound)?;
        let position_ms = match position_ms {
            Some(position_ms) => position_ms,
            None => self.start_position(user_id, episode_id).await?,
        };
        let episode = self
            .episode_for(user_id, episode_id)
            .await?
            .ok_or(ConnectError::NotFound)?;
        let player = sonos
            .play(
                &self.pool,
                user_id,
                player_id,
                SonosPlayRequest {
                    episode_id,
                    position_ms,
                },
            )
            .await?;
        let now = Instant::now();
        let mut effects = Effects::default();
        let mut accounts = self.accounts();
        let account = accounts.entry(user_id).or_default();
        let session = match account.active() {
            // The Sonos change may have come first and named the session.
            Some(Active::Sonos {
                player_id: active,
                session,
            }) if active == player_id => *session,
            _ => None,
        };
        account.playback = Some(Current {
            active: account.active().cloned(),
            episode,
            position_ms,
            playing: true,
            speed: 1.0,
            volume: Some(player.volume),
            at: now,
        });
        Self::claim(
            account,
            Active::Sonos {
                player_id: player_id.to_owned(),
                session,
            },
            &mut effects,
        );
        let gone = Self::broadcast(account);
        effects.gone.extend(gone.gone);
        let view = account.view(now).ok_or(ConnectError::NoSession)?;
        drop(accounts);
        self.apply(user_id, effects);
        Ok(view)
    }

    /// Spec 8's start position over the server's `episode_state`: the
    /// stored position, or 0 for a played episode, which is then written
    /// unplayed as the replay rule says.
    async fn start_position(&self, user_id: UserId, episode_id: EpisodeId) -> Result<i64> {
        let row = sqlx::query!(
            "select position_ms, played from episode_state
             where user_id = $1 and episode_id = $2",
            user_id.as_uuid(),
            episode_id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await
        .context("query the episode state")?;
        match row {
            Some(row) if row.played => {
                write_episode_state(
                    &self.pool,
                    user_id,
                    episode_id,
                    &[EpisodeWrite::Played(false)],
                    Stamp {
                        at: self.clock.now(),
                        dev: DeviceId::new_v7(),
                    },
                )
                .await
                .context("write the replay")?;
                Ok(0)
            }
            Some(row) => Ok(row.position_ms),
            None => Ok(0),
        }
    }

    // ==========================================================================
    // Sonos sessions
    // ==========================================================================

    /// Follows a Sonos session change (spec 14.1): a start claims the
    /// playback, updates move it, an end leaves it with no active device.
    async fn sonos_changed(self: &Arc<Self>, change: SonosChange) -> Result<()> {
        let now = Instant::now();
        let mut effects = Effects::default();
        let user_id = match &change {
            SonosChange::Started { user_id, .. }
            | SonosChange::Updated { user_id, .. }
            | SonosChange::Ended { user_id, .. } => *user_id,
        };
        let started_episode = match &change {
            SonosChange::Started { episode_id, .. } => {
                self.episode_for(user_id, *episode_id).await?
            }
            _ => None,
        };
        let mut accounts = self.accounts();
        let account = accounts.entry(user_id).or_default();
        let ours = |account: &Account, player: &str, session: DeviceId| {
            matches!(account.active(), Some(Active::Sonos { player_id, session: known })
                if player_id == player && known.is_none_or(|known| known == session))
        };
        match change {
            SonosChange::Started {
                player_id,
                session,
                position_ms,
                ..
            } => {
                let Some(episode) = started_episode else {
                    return Ok(());
                };
                let replaces = account.active().cloned();
                let volume = account.playback.as_ref().and_then(|current| current.volume);
                account.playback = Some(Current {
                    active: replaces,
                    episode,
                    position_ms,
                    playing: true,
                    speed: 1.0,
                    volume,
                    at: now,
                });
                Self::claim(
                    account,
                    Active::Sonos {
                        player_id,
                        session: Some(session),
                    },
                    &mut effects,
                );
            }
            SonosChange::Updated {
                player_id,
                session,
                position_ms,
                playing,
                volume,
                ..
            } => {
                if !ours(account, &player_id, session) {
                    return Ok(());
                }
                let current = account.playback.as_mut().expect("ours implies a playback");
                current.active = Some(Active::Sonos {
                    player_id,
                    session: Some(session),
                });
                current.position_ms = position_ms;
                current.playing = playing;
                current.at = now;
                if volume.is_some() {
                    current.volume = volume;
                }
            }
            SonosChange::Ended {
                player_id,
                session,
                position_ms,
                ..
            } => {
                if !ours(account, &player_id, session) {
                    return Ok(());
                }
                let current = account.playback.as_mut().expect("ours implies a playback");
                current.active = None;
                current.position_ms = position_ms;
                current.playing = false;
                current.volume = None;
                current.at = now;
                account.generation += 1;
            }
        }
        let gone = Self::broadcast(account);
        effects.gone.extend(gone.gone);
        drop(accounts);
        self.apply(user_id, effects);
        Ok(())
    }
}

/// Feeds Sonos session changes to the hub, in order, until either goes away.
async fn follow_sonos(
    connect: std::sync::Weak<Connect>,
    mut changes: mpsc::UnboundedReceiver<SonosChange>,
) {
    while let Some(change) = changes.recv().await {
        let Some(connect) = connect.upgrade() else {
            return;
        };
        if let Err(error) = connect.sonos_changed(change).await {
            tracing::error!(
                error = format!("{error:#}"),
                "following a Sonos session change failed"
            );
        }
    }
}
