//! Playback on Sonos (spec 13): the players, the operations behind
//! `/v1/sonos/*`, and the tracker that polls every session's group and
//! writes its listening state.
//!
//! [`Sonos`] exists only when `sonos_hosts` names at least one speaker. It
//! owns the HTTP client, the seed speakers and a short-lived topology
//! cache. Sessions live in `sonos_session`, so a restart picks them up
//! again; the only in-memory state per session is how long its group has
//! been unreachable.

pub mod topology;
pub mod tracker;
pub mod upnp;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Report, Result, WrapErr};
use pod_protocol::settings::{DEFAULT_PLAYED_THRESHOLD_MS, PLAYED_THRESHOLD_MS};
use pod_protocol::{
    DeviceId, EpisodeId, SonosCommand, SonosPlayRequest, SonosPlayer, SonosPlayerState,
    SonosReleased, SonosSessionView, Stamp, Timestamp, UserId,
};
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::sync::push::{EpisodeWrite, write_episode_state};
use topology::Group;
use tracker::{Next, Observation, Seen, Session, Tracked, TransportState};
use upnp::{Speaker, TrackMetadata, UpnpError};

/// How long a fetched topology serves requests and polls. A failed command
/// or poll drops it sooner.
const TOPOLOGY_TTL: Duration = Duration::from_secs(10);

/// How often the tracker polls every session.
const TICK: Duration = Duration::from_secs(5);

// ==============================================================================
// Errors
// ==============================================================================

/// Why a Sonos operation failed, by the response it becomes.
#[derive(Debug, thiserror::Error)]
pub enum SonosError {
    /// `404 not_found`: the player isn't a current coordinator, or the
    /// episode isn't readable by the user.
    #[error("no such player or readable episode")]
    NotFound,
    /// `409 no_session`: the user has no session on the player.
    #[error("the user has no session on the player")]
    NoSession,
    /// `502 upstream_unavailable`.
    #[error("the speaker failed")]
    Speaker(#[source] UpnpError),
    /// `500 internal`.
    #[error("{0:#}")]
    Internal(Report),
}

impl From<Report> for SonosError {
    fn from(report: Report) -> Self {
        Self::Internal(report)
    }
}

// ==============================================================================
// The service
// ==============================================================================

#[derive(Debug)]
pub struct Sonos {
    /// Never empty: [`Sonos::new`] returns `None` instead.
    seeds: Vec<Url>,
    http: reqwest::Client,
    topology: Mutex<Option<CachedTopology>>,
    /// Since when each session's group hasn't answered, by the session's
    /// device id. Only the tracker reads it, and losing it on a restart only
    /// restarts the two-minute clock.
    unreachable_since: Mutex<HashMap<DeviceId, Timestamp>>,
}

#[derive(Debug)]
struct CachedTopology {
    fetched_at: Instant,
    groups: Arc<[Group]>,
}

impl Sonos {
    /// The service for the speakers at `seeds` (base URLs, see
    /// [`crate::config::Config::sonos_hosts`]), or `None` when there are
    /// none, which turns Sonos off.
    pub fn new(seeds: Vec<Url>) -> Result<Option<Sonos>> {
        if seeds.is_empty() {
            return Ok(None);
        }
        Ok(Some(Sonos {
            seeds,
            http: upnp::http_client().context("build the Sonos HTTP client")?,
            topology: Mutex::new(None),
            unreachable_since: Mutex::new(HashMap::new()),
        }))
    }

    fn speaker(&self, group: &Group) -> Speaker {
        Speaker::new(self.http.clone(), group.base.clone())
    }

    /// Drops the cached topology, so the next lookup asks a seed again. A
    /// speaker that failed may have left its group or the network.
    fn failed(&self, error: UpnpError) -> SonosError {
        *lock(&self.topology) = None;
        SonosError::Speaker(error)
    }

    // ==========================================================================
    // Topology
    // ==========================================================================

    async fn groups(&self) -> Result<Arc<[Group]>, SonosError> {
        if let Some(cached) = lock(&self.topology).as_ref()
            && cached.fetched_at.elapsed() < TOPOLOGY_TTL
        {
            return Ok(Arc::clone(&cached.groups));
        }
        self.fetch_topology().await
    }

    /// Asks the seeds in order until one answers. Any speaker reports the
    /// whole household, so one is enough.
    async fn fetch_topology(&self) -> Result<Arc<[Group]>, SonosError> {
        let mut last_error = None;
        for seed in &self.seeds {
            let speaker = Speaker::new(self.http.clone(), seed.clone());
            let result = speaker.zone_group_state().await.and_then(|state| {
                topology::parse_zone_groups(&state).map_err(|error| UpnpError::Body {
                    action: "GetZoneGroupState",
                    detail: error.to_string(),
                })
            });
            match result {
                Ok(groups) => {
                    let groups: Arc<[Group]> = groups.into();
                    *lock(&self.topology) = Some(CachedTopology {
                        fetched_at: Instant::now(),
                        groups: Arc::clone(&groups),
                    });
                    return Ok(groups);
                }
                Err(error) => {
                    tracing::debug!(%seed, error = %error, "a Sonos seed didn't answer");
                    last_error = Some(error);
                }
            }
        }
        Err(SonosError::Speaker(
            last_error.expect("Sonos::new refuses an empty seed list"),
        ))
    }

    /// The group whose coordinator is `player_id`. A miss refetches once,
    /// since a group formed in the last few seconds isn't cached yet.
    async fn find(&self, player_id: &str) -> Result<Group, SonosError> {
        let cached = self.groups().await?;
        if let Some(group) = cached.iter().find(|group| group.id == player_id) {
            return Ok(group.clone());
        }
        self.fetch_topology()
            .await?
            .iter()
            .find(|group| group.id == player_id)
            .cloned()
            .ok_or(SonosError::NotFound)
    }

    // ==========================================================================
    // Players
    // ==========================================================================

    /// Every group as the user sees it. A group whose coordinator doesn't
    /// answer is left out rather than failing the list: one unplugged
    /// speaker shouldn't hide the others.
    pub async fn players(
        &self,
        pool: &PgPool,
        user_id: UserId,
    ) -> Result<Vec<SonosPlayer>, SonosError> {
        let groups = self.groups().await?;
        let sessions = load_sessions(pool, None)
            .await
            .context("load the Sonos sessions")?;
        let mut tasks = JoinSet::new();
        for (index, group) in groups.iter().enumerate() {
            let speaker = self.speaker(group);
            let session = sessions
                .iter()
                .find(|session| session.player_id == group.id)
                .cloned();
            tasks.spawn(async move {
                let status = group_status(&speaker, session.as_ref()).await;
                (index, session, status)
            });
        }
        let mut players = Vec::with_capacity(groups.len());
        for (index, session, status) in tasks.join_all().await {
            let group = &groups[index];
            match status {
                Ok((volume, seen)) => players.push((
                    index,
                    player_view(group, user_id, session.as_ref(), volume, &seen),
                )),
                Err(error) => {
                    tracing::warn!(player_id = group.id, error = %error, "a Sonos group didn't answer");
                    *lock(&self.topology) = None;
                }
            }
        }
        players.sort_by_key(|(index, _)| *index);
        Ok(players.into_iter().map(|(_, player)| player).collect())
    }

    /// One player as the user sees it, for the responses to `play` and
    /// `command`.
    async fn player(
        &self,
        pool: &PgPool,
        user_id: UserId,
        group: &Group,
    ) -> Result<SonosPlayer, SonosError> {
        let session = load_sessions(pool, Some(&group.id))
            .await
            .context("load the player's session")?
            .pop();
        let (volume, seen) = group_status(&self.speaker(group), session.as_ref())
            .await
            .map_err(|error| self.failed(error))?;
        Ok(player_view(group, user_id, session.as_ref(), volume, &seen))
    }

    // ==========================================================================
    // Starting a session
    // ==========================================================================

    /// Loads the episode on the group, seeks, plays, and records the
    /// session (spec 13.3, "Starting"). Whatever the group did before stops,
    /// another session on it included, which ends here: its row is
    /// replaced, and its last position was written by its last poll.
    pub async fn play(
        &self,
        pool: &PgPool,
        user_id: UserId,
        player_id: &str,
        request: SonosPlayRequest,
    ) -> Result<SonosPlayer, SonosError> {
        let episode = load_playable(pool, user_id, request.episode_id)
            .await
            .context("load the episode to play")?
            .ok_or(SonosError::NotFound)?;
        let group = self.find(player_id).await?;
        let speaker = self.speaker(&group);
        let metadata = upnp::didl_lite(&TrackMetadata {
            audio_url: &episode.audio_url,
            mime_type: Some(episode.audio_type.as_str()).filter(|mime| !mime.trim().is_empty()),
            title: &episode.title,
            show: episode.show.as_deref(),
            artwork_url: episode.artwork_url.as_deref(),
        });
        speaker
            .set_av_transport_uri(&episode.audio_url, &metadata)
            .await
            .map_err(|error| self.failed(error))?;
        // Seeking before playing keeps playback from starting at 0:00 and
        // jumping. A speaker that refuses to seek while stopped gets the
        // seek once it plays.
        let seek_later = match speaker.seek(request.position_ms).await {
            Ok(()) => false,
            Err(UpnpError::Fault { code, .. }) => {
                tracing::debug!(
                    player_id,
                    ?code,
                    "the speaker refused to seek before playing"
                );
                true
            }
            Err(error) => return Err(self.failed(error)),
        };
        speaker.play().await.map_err(|error| self.failed(error))?;
        if seek_later && let Err(error) = speaker.seek(request.position_ms).await {
            // Playing from the start beats failing a request whose episode
            // already plays; the tracker reports the real position.
            tracing::warn!(player_id, error = %error, "the speaker refused to seek after playing");
        }
        let session = Session {
            player_id: group.id.clone(),
            user_id,
            episode_id: request.episode_id,
            audio_url: episode.audio_url,
            device_id: DeviceId::new_v7(),
            duration_ms: episode.duration_ms,
            position_ms: request.position_ms,
            // Only a poll that sees PLAYING arms the end-of-episode rule. A
            // stream the speaker can't fetch goes TRANSITIONING, then
            // STOPPED, and must not count as finished.
            playing: false,
            played_written: false,
        };
        insert_session(pool, &session)
            .await
            .context("record the Sonos session")?;
        tracing::info!(player_id, episode_id = %session.episode_id, "Sonos session started");
        self.player(pool, user_id, &group).await
    }

    // ==========================================================================
    // Commands
    // ==========================================================================

    /// Runs a command on the group (spec 13.2). Everything but `volume`
    /// needs the user's own session there, and updates the session row and
    /// writes the position at once, so the next poll sees no stale jump.
    pub async fn command(
        &self,
        pool: &PgPool,
        user_id: UserId,
        player_id: &str,
        command: SonosCommand,
        now: Timestamp,
    ) -> Result<SonosPlayer, SonosError> {
        let group = self.find(player_id).await?;
        let speaker = self.speaker(&group);
        let failed = |error| self.failed(error);
        if let SonosCommand::Volume { volume } = command {
            speaker.set_group_volume(volume).await.map_err(failed)?;
            return self.player(pool, user_id, &group).await;
        }

        let mut session = own_session(pool, user_id, &group.id).await?;
        // The group may have switched source since the last poll. A command
        // must not pause or seek whatever the Sonos app put there, so the
        // session ends here as the next poll would end it.
        let seen = observe(&speaker, &session.audio_url)
            .await
            .map_err(failed)?;
        if !seen.ours {
            delete_session(pool, session.device_id)
                .await
                .context("end the Sonos session")?;
            return Err(SonosError::NoSession);
        }
        let (position_ms, playing) = match command {
            // Pausing a group that doesn't play is a UPnP fault (701), and
            // a stopped group reports position 0, which mustn't be written.
            SonosCommand::Pause if live(seen.state) => {
                speaker.pause().await.map_err(failed)?;
                (speaker.position_ms().await.map_err(failed)?, false)
            }
            SonosCommand::Pause => (seen.position_for(&session), false),
            SonosCommand::Resume => {
                speaker.play().await.map_err(failed)?;
                (speaker.position_ms().await.map_err(failed)?, true)
            }
            SonosCommand::Seek { position_ms } => {
                speaker.seek(position_ms).await.map_err(failed)?;
                (position_ms, session.playing)
            }
            SonosCommand::Skip { delta_ms } => {
                let current = speaker.position_ms().await.map_err(failed)?;
                let position_ms = current.saturating_add(delta_ms).max(0);
                speaker.seek(position_ms).await.map_err(failed)?;
                (position_ms, session.playing)
            }
            SonosCommand::Volume { .. } => unreachable!("handled above"),
        };
        session.position_ms = position_ms;
        session.playing = playing;
        update_session(pool, &session)
            .await
            .context("update the Sonos session")?;
        write_position(pool, &session, now).await?;
        self.player(pool, user_id, &group).await
    }

    /// Pauses the group, writes the position and ends the session, answering
    /// with the session as it was, so the client continues from there.
    pub async fn release(
        &self,
        pool: &PgPool,
        user_id: UserId,
        player_id: &str,
        now: Timestamp,
    ) -> Result<SonosReleased, SonosError> {
        let group = self.find(player_id).await?;
        let mut session = own_session(pool, user_id, &group.id).await?;
        let speaker = self.speaker(&group);
        let seen = observe(&speaker, &session.audio_url)
            .await
            .map_err(|error| self.failed(error))?;
        let playing = seen.playing_for(&session);
        // A group that already switched to something else keeps playing it.
        if seen.ours && live(seen.state) {
            speaker.pause().await.map_err(|error| self.failed(error))?;
        }
        if seen.ours {
            session.position_ms = seen.position_for(&session);
            write_position(pool, &session, now).await?;
        }
        delete_session(pool, session.device_id)
            .await
            .context("end the Sonos session")?;
        tracing::info!(player_id, episode_id = %session.episode_id, "Sonos session released");
        Ok(SonosReleased {
            episode_id: session.episode_id,
            position_ms: session.position_ms,
            playing,
        })
    }

    // ==========================================================================
    // Tracker
    // ==========================================================================

    /// One tracker pass (spec 13.3): polls every session's coordinator at
    /// once, then applies [`tracker::step`] to each. A session whose group
    /// no longer holds its URI ends on its first pass, which is how a
    /// restart drops sessions that ended while the server was down.
    ///
    /// `now` stamps the writes. Failures of single sessions are logged and
    /// leave the others alone.
    pub async fn tick(&self, pool: &PgPool, now: Timestamp) -> Result<()> {
        let sessions = load_sessions(pool, None)
            .await
            .context("load the Sonos sessions")?;
        if sessions.is_empty() {
            return Ok(());
        }
        let groups = match self.groups().await {
            Ok(groups) => Some(groups),
            Err(error) => {
                tracing::warn!(error = %error, "no Sonos seed answered");
                None
            }
        };

        let mut polls = JoinSet::new();
        for session in sessions {
            let group = groups.as_ref().map(|groups| {
                groups
                    .iter()
                    .find(|group| group.id == session.player_id)
                    .map(|group| self.speaker(group))
            });
            polls.spawn(async move {
                let observation = match group {
                    None => Observation::Unreachable,
                    Some(None) => Observation::NotCoordinator,
                    Some(Some(speaker)) => match observe(&speaker, &session.audio_url).await {
                        Ok(seen) => Observation::Seen(seen),
                        Err(error) => {
                            tracing::warn!(player_id = session.player_id, error = %error,
                                "a Sonos session's group didn't answer");
                            Observation::Unreachable
                        }
                    },
                };
                (session, observation)
            });
        }
        for (session, observation) in polls.join_all().await {
            if observation == Observation::Unreachable {
                *lock(&self.topology) = None;
            }
            let player_id = session.player_id.clone();
            if let Err(error) = self.track(pool, session, observation, now).await {
                tracing::error!(
                    player_id,
                    error = format!("{error:#}"),
                    "tracking a Sonos session failed"
                );
            }
        }
        Ok(())
    }

    async fn track(
        &self,
        pool: &PgPool,
        session: Session,
        observation: Observation,
        now: Timestamp,
    ) -> Result<()> {
        let tracked = Tracked {
            unreachable_since: lock(&self.unreachable_since)
                .get(&session.device_id)
                .copied(),
            session,
        };
        let threshold_ms = played_threshold_ms(pool, tracked.session.user_id)
            .await
            .context("load the played threshold")?;
        let step = tracker::step(&tracked, observation, now, threshold_ms);
        let session = &tracked.session;
        let writes: Vec<EpisodeWrite> = step
            .writes
            .iter()
            .map(|write| match write {
                tracker::Write::Position(position_ms) => EpisodeWrite::Position(*position_ms),
                tracker::Write::Played => EpisodeWrite::Played(true),
            })
            .collect();
        if !writes.is_empty() {
            write_episode_state(
                pool,
                session.user_id,
                session.episode_id,
                &writes,
                Stamp {
                    at: now,
                    dev: session.device_id,
                },
            )
            .await
            .context("write the session's episode state")?;
        }
        match step.next {
            Next::Keep(next) => {
                {
                    let mut unreachable = lock(&self.unreachable_since);
                    match next.unreachable_since {
                        Some(since) => unreachable.insert(session.device_id, since),
                        None => unreachable.remove(&session.device_id),
                    };
                }
                if next.session != *session {
                    update_session(pool, &next.session)
                        .await
                        .context("update the Sonos session")?;
                }
            }
            Next::End => {
                lock(&self.unreachable_since).remove(&session.device_id);
                delete_session(pool, session.device_id)
                    .await
                    .context("end the Sonos session")?;
                tracing::info!(
                    player_id = session.player_id,
                    episode_id = %session.episode_id,
                    ?observation,
                    "Sonos session ended"
                );
            }
        }
        Ok(())
    }
}

/// Runs [`Sonos::tick`] every 5 seconds, starting at once, until
/// `shutdown`. A pass that fails is logged and the next one tries again.
pub async fn run(sonos: Arc<Sonos>, pool: PgPool, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = interval.tick() => {}
        }
        if let Err(error) = sonos.tick(&pool, Timestamp::now()).await {
            tracing::error!(
                error = format!("{error:#}"),
                "the Sonos tracker pass failed"
            );
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Nothing panics while holding these locks (plain assignments and map
    // operations), and the data stays valid either way.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ==============================================================================
// Speaker queries
// ==============================================================================

/// One poll of a session's group: transport state, URI and position.
/// The group plays, or is about to: the states a `Pause` applies to.
fn live(state: TransportState) -> bool {
    matches!(
        state,
        TransportState::Playing | TransportState::Transitioning
    )
}

async fn observe(speaker: &Speaker, audio_url: &str) -> Result<Seen, UpnpError> {
    let (state, uri, position_ms) = tokio::join!(
        speaker.transport_state(),
        speaker.current_uri(),
        speaker.position_ms()
    );
    let (state, uri) = (state?, uri?);
    let ours = uri == audio_url;
    // `RelTime` only matters while the group holds the session's episode;
    // for line-in or TV audio it may not even be a time.
    let position_ms = match position_ms {
        Ok(position_ms) => position_ms,
        Err(_) if !ours => 0,
        Err(error) => return Err(error),
    };
    Ok(Seen {
        state,
        ours,
        position_ms,
    })
}

/// The group volume and what the group does: a full poll when a session is
/// on it, the transport state alone otherwise.
async fn group_status(
    speaker: &Speaker,
    session: Option<&Session>,
) -> Result<(u8, Seen), UpnpError> {
    let seen = async {
        match session {
            Some(session) => observe(speaker, &session.audio_url).await,
            None => Ok(Seen {
                state: speaker.transport_state().await?,
                ours: false,
                position_ms: 0,
            }),
        }
    };
    let (volume, seen) = tokio::join!(speaker.group_volume(), seen);
    Ok((volume?, seen?))
}

/// A player as `user_id` sees it (spec 13.2): `pod` with the session when
/// it is theirs, `other` when someone else's session or anything else
/// holds the group, `idle` otherwise.
fn player_view(
    group: &Group,
    user_id: UserId,
    session: Option<&Session>,
    volume: u8,
    seen: &Seen,
) -> SonosPlayer {
    let (state, session) = match session {
        Some(session) if session.user_id == user_id => (
            SonosPlayerState::Pod,
            Some(SonosSessionView {
                episode_id: session.episode_id,
                playing: seen.playing_for(session),
                position_ms: seen.position_for(session),
            }),
        ),
        Some(_) => (SonosPlayerState::Other, None),
        None if matches!(
            seen.state,
            TransportState::Stopped | TransportState::NoMediaPresent
        ) =>
        {
            (SonosPlayerState::Idle, None)
        }
        None => (SonosPlayerState::Other, None),
    };
    SonosPlayer {
        id: group.id.clone(),
        name: group.name.clone(),
        volume,
        state,
        session,
    }
}

// ==============================================================================
// Database
// ==============================================================================

/// What the speaker needs to play an episode and show it.
#[derive(Debug)]
struct Playable {
    audio_url: String,
    audio_type: String,
    title: String,
    show: Option<String>,
    artwork_url: Option<String>,
    duration_ms: Option<i64>,
}

/// The episode if the user may play it: not deleted, and in a podcast the
/// user can read (spec 4.5).
async fn load_playable(
    pool: &PgPool,
    user_id: UserId,
    episode_id: EpisodeId,
) -> Result<Option<Playable>> {
    let row = sqlx::query!(
        r#"select e.audio_url, e.audio_type, e.title, e.duration_ms,
                  coalesce(e.artwork_url, p.artwork_url) as artwork_url, p.title as show
           from episode e
           join podcast p on p.id = e.podcast_id
           join subscription s on s.podcast_id = p.id
           where e.id = $2 and not e.deleted
             and s.user_id = $1 and s.subscribed and s.replaced_by is null"#,
        user_id.as_uuid(),
        episode_id.as_uuid(),
    )
    .fetch_optional(pool)
    .await
    .context("query the episode")?;
    Ok(row.map(|row| Playable {
        audio_url: row.audio_url,
        audio_type: row.audio_type,
        title: row.title,
        show: row.show,
        artwork_url: row.artwork_url,
        duration_ms: row.duration_ms,
    }))
}

/// Every session, or the one on `player_id`.
async fn load_sessions(pool: &PgPool, player_id: Option<&str>) -> Result<Vec<Session>> {
    let rows = sqlx::query!(
        "select player_id, user_id, episode_id, audio_url, device_id, duration_ms,
                position_ms, playing, played_written
         from sonos_session where $1::text is null or player_id = $1",
        player_id,
    )
    .fetch_all(pool)
    .await
    .context("query the Sonos sessions")?;
    Ok(rows
        .into_iter()
        .map(|row| Session {
            player_id: row.player_id,
            user_id: UserId::from_uuid(row.user_id),
            episode_id: EpisodeId::from_uuid(row.episode_id),
            audio_url: row.audio_url,
            device_id: DeviceId::from_uuid(row.device_id),
            duration_ms: row.duration_ms,
            position_ms: row.position_ms,
            playing: row.playing,
            played_written: row.played_written,
        })
        .collect())
}

/// The user's session on the player, or [`SonosError::NoSession`].
async fn own_session(
    pool: &PgPool,
    user_id: UserId,
    player_id: &str,
) -> Result<Session, SonosError> {
    load_sessions(pool, Some(player_id))
        .await
        .context("load the player's session")?
        .into_iter()
        .find(|session| session.user_id == user_id)
        .ok_or(SonosError::NoSession)
}

/// Records a new session, replacing whatever session the player had.
async fn insert_session(pool: &PgPool, session: &Session) -> Result<()> {
    sqlx::query!(
        "insert into sonos_session (player_id, user_id, episode_id, audio_url, device_id,
             duration_ms, position_ms, playing, played_written)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         on conflict (player_id) do update set
             user_id = excluded.user_id, episode_id = excluded.episode_id,
             audio_url = excluded.audio_url, device_id = excluded.device_id,
             duration_ms = excluded.duration_ms, position_ms = excluded.position_ms,
             playing = excluded.playing, played_written = excluded.played_written,
             started_at = now()",
        session.player_id,
        session.user_id.as_uuid(),
        session.episode_id.as_uuid(),
        session.audio_url,
        session.device_id.as_uuid(),
        session.duration_ms,
        session.position_ms,
        session.playing,
        session.played_written,
    )
    .execute(pool)
    .await
    .context("upsert the session row")?;
    Ok(())
}

/// Stores the session's observed fields. Keyed by the device id, so a
/// pass that raced a new `play` on the same player leaves the new session
/// alone.
async fn update_session(pool: &PgPool, session: &Session) -> Result<()> {
    sqlx::query!(
        "update sonos_session set position_ms = $2, playing = $3, played_written = $4
         where device_id = $1",
        session.device_id.as_uuid(),
        session.position_ms,
        session.playing,
        session.played_written,
    )
    .execute(pool)
    .await
    .context("update the session row")?;
    Ok(())
}

async fn delete_session(pool: &PgPool, device_id: DeviceId) -> Result<()> {
    sqlx::query!(
        "delete from sonos_session where device_id = $1",
        device_id.as_uuid()
    )
    .execute(pool)
    .await
    .context("delete the session row")?;
    Ok(())
}

/// Writes the session's position as of `now`, stamped with its device id.
async fn write_position(pool: &PgPool, session: &Session, now: Timestamp) -> Result<()> {
    write_episode_state(
        pool,
        session.user_id,
        session.episode_id,
        &[EpisodeWrite::Position(session.position_ms)],
        Stamp {
            at: now,
            dev: session.device_id,
        },
    )
    .await
    .context("write the session's position")
}

/// The user's `played_threshold_ms`, or its default when unset or not a
/// non-negative integer (spec 2.4: a wrong type reads as the default).
async fn played_threshold_ms(pool: &PgPool, user_id: UserId) -> Result<i64> {
    let stored = sqlx::query_scalar!(
        "select value from user_setting where user_id = $1 and key = $2",
        user_id.as_uuid(),
        PLAYED_THRESHOLD_MS,
    )
    .fetch_optional(pool)
    .await
    .context("query the setting")?;
    Ok(stored
        .as_ref()
        .and_then(serde_json::Value::as_i64)
        .filter(|threshold_ms| *threshold_ms >= 0)
        .unwrap_or(DEFAULT_PLAYED_THRESHOLD_MS))
}
