//! Where the episode plays: this device or a Sonos group (spec 13.4).
//!
//! On a group, pod-server plays the episode and writes its listening state,
//! so the player's phase is [`Phase::Remote`]: mpv is stopped, the contract
//! is gone and nothing here writes listening state. The player's commands
//! go to the server as Sonos commands instead, and a follow task polls the
//! group and runs the bar's clock. The pure decisions are
//! [`crate::sonos::remote`]; this module sends the requests (on the tokio
//! side, never on the UI thread) and applies their answers.
//!
//! Each hand-off gets a generation. An answer for an older one, which the
//! user has since moved on from, changes nothing.

use std::time::{Duration, Instant};

use gpui_kit::{Context, Task};
use pod_protocol::{EpisodeId, SonosCommand, SonosPlayRequest, SonosPlayer};
use tokio::sync::oneshot;
use tokio::task::JoinError;

use super::contract::{Contract, Stored, Write};
use super::mpv::Command;
use super::{Current, Job, Phase, Player, SonosNotice};
use crate::runtime::Background;
use crate::sonos::remote::{Seen, Verdict, hand_off_start, observe};
use crate::sonos::{Remote, SonosError, Target};
use crate::sync::SyncHandle;

/// How often the follow task runs the clock and checks whether a poll is
/// due. The bar shows whole seconds.
const TICK: Duration = Duration::from_millis(500);

/// What follows a release.
#[derive(Debug)]
enum AfterRelease {
    /// "This device": go on here, playing or paused as the group was.
    Here,
    /// Another group: the episode goes on there.
    HandOff(Target),
}

impl Player {
    /// Whether the server lists any Sonos group, as last asked.
    pub fn sonos_available(&self) -> bool {
        self.sonos_available
    }

    pub(super) fn is_remote(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|current| current.remote().is_some())
    }

    /// Lists the groups for the output picker, empty when there are none or
    /// the server can't be asked. Keeps [`Player::sonos_available`] current.
    pub fn list_players(&mut self, cx: &mut Context<Self>) -> Task<Vec<SonosPlayer>> {
        let api = self.sonos.clone();
        let listed = cx
            .global::<Background>()
            .spawn(async move { api.players().await });
        cx.spawn(async move |this, cx| {
            let players = joined(listed.await).unwrap_or_else(|error| {
                tracing::info!("listing the Sonos groups failed: {error}");
                Vec::new()
            });
            let _ = this.update(cx, |this, cx| {
                this.sonos_available = !players.is_empty();
                cx.notify();
            });
            players
        })
    }

    /// Asks whether the account has Sonos groups, and picks up a session of
    /// the parked episode that played on while the app was closed.
    pub(super) fn refresh_sonos(&mut self, cx: &mut Context<Self>) {
        if self.sonos_account.is_none() {
            self.sonos_available = false;
            return;
        }
        let listed = self.list_players(cx);
        cx.spawn(async move |this, cx| {
            let players = listed.await;
            let _ = this.update(cx, |this, cx| {
                let Some(current) = &this.current else {
                    return;
                };
                // TODO: follow a session of another episode, which would
                // replace the parked one.
                if !matches!(current.phase, Phase::Parked) {
                    return;
                }
                if let Seen::Session(player, session) = observe(&players, current.episode.id, None)
                {
                    let remote = Remote::following(player, session, Instant::now());
                    this.enter_remote(remote, true, cx);
                }
            });
        })
        .detach();
    }

    // ==========================================================================
    // Handing off and taking back
    // ==========================================================================

    /// Hands the episode to `target`. The local play ends with its ordinary
    /// position write, and the group starts where a local play would have
    /// (spec 8). From another group, the episode goes on where that one
    /// released it.
    pub fn hand_off(&mut self, target: Target, cx: &mut Context<Self>) {
        let Some(current) = &self.current else {
            return;
        };
        if let Some(remote) = current.remote() {
            if !remote.is_connecting() && remote.target != target {
                self.release(AfterRelease::HandOff(target), cx);
            }
            return;
        }
        let id = current.episode.id;
        if current.session().is_some() {
            self.send(Command::Stop);
        }
        self.end_session(Contract::stop);
        self.play_on(target, id, cx);
    }

    /// "This device": takes the episode back from its group and goes on
    /// here, playing or paused as it was there. Not a replay.
    pub fn take_back(&mut self, cx: &mut Context<Self>) {
        self.release(AfterRelease::Here, cx);
    }

    /// Starts `id` on `target`, from the state its local writes left. Shows
    /// "Connecting" at once, so no command reaches mpv meanwhile.
    pub(super) fn play_on(&mut self, target: Target, id: EpisodeId, cx: &mut Context<Self>) {
        let Some(current) = &self.current else {
            return;
        };
        let shown = Remote::connecting(target.clone(), current.position_ms(), Instant::now());
        let generation = self.enter_remote(shown, false, cx);
        // Read after the writes queued so far, the stop's included.
        let loaded = self.load(Some(id));
        cx.spawn(async move |this, cx| {
            let episode = loaded.await.ok().flatten();
            let _ = this.update(cx, |this, cx| match episode {
                Some(episode) => this.connect(generation, target, episode, cx),
                None => {
                    tracing::warn!("episode {id} to hand off isn't in the store");
                    this.end_remote(generation, cx);
                }
            });
        })
        .detach();
    }

    /// Sends the play request for `episode` once its local writes are
    /// pushed: the server starts from its own copy of the episode's state.
    fn connect(
        &mut self,
        generation: u64,
        target: Target,
        episode: super::Episode,
        cx: &mut Context<Self>,
    ) {
        if self.generation() != Some(generation) {
            return;
        }
        let start = hand_off_start(episode.stored);
        let id = episode.id;
        let remote = Remote::connecting(target.clone(), start.position_ms, Instant::now());
        self.current = Some(Current::new(
            episode,
            start.position_ms,
            Phase::Remote {
                remote,
                generation,
                follow: None,
            },
        ));
        if start.unplay {
            self.issue(vec![Write::Played(false)]);
        }
        let _ = self.jobs.send(Job::Remember(id));
        let (reply, written) = oneshot::channel();
        let _ = self.jobs.send(Job::Barrier(reply));

        let (engine, api) = (self.engine.clone(), self.sonos.clone());
        let request = SonosPlayRequest {
            episode_id: id,
            position_ms: start.position_ms,
        };
        let answer = cx.global::<Background>().spawn(async move {
            let _ = written.await;
            // A failed push leaves the writes queued for the next sync. The
            // group still starts at the position sent along.
            match engine.sync().await {
                Ok(state) => tracing::debug!(?state, "pushed the outbox before a hand-off"),
                Err(error) => tracing::warn!("pushing before a hand-off failed: {error:#}"),
            }
            api.play(&target.id, request).await
        });
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| this.connected(generation, answer, cx));
        })
        .detach();
        cx.notify();
    }

    fn connected(
        &mut self,
        generation: u64,
        answer: Result<Result<SonosPlayer, SonosError>, JoinError>,
        cx: &mut Context<Self>,
    ) {
        let answer = joined(answer);
        let Some((remote, id, _)) = self.remote_parts(generation) else {
            return;
        };
        match answer {
            Ok(player) => {
                remote.connected(&player, id, Instant::now());
                let follow = self.follow(generation, cx);
                if let Some(Current {
                    phase: Phase::Remote { follow: slot, .. },
                    ..
                }) = &mut self.current
                {
                    *slot = Some(follow);
                }
            }
            Err(error) => {
                // The group never started, so the episode stays here,
                // paused where it would have started there.
                let message = error.message(&remote.target.name);
                self.park(None);
                cx.emit(SonosNotice(message));
            }
        }
        cx.notify();
    }

    /// Ends the session on its group, then goes on as `then` says.
    fn release(&mut self, then: AfterRelease, cx: &mut Context<Self>) {
        let Some(generation) = self.generation() else {
            return;
        };
        let Some((remote, _, _)) = self.remote_parts(generation) else {
            return;
        };
        if remote.is_connecting() {
            return;
        }
        let player_id = remote.target.id.clone();
        let api = self.sonos.clone();
        let answer = cx
            .global::<Background>()
            .spawn(async move { api.release(&player_id).await });
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| this.released(generation, then, answer, cx));
        })
        .detach();
    }

    fn released(
        &mut self,
        generation: u64,
        then: AfterRelease,
        answer: Result<Result<pod_protocol::SonosReleased, SonosError>, JoinError>,
        cx: &mut Context<Self>,
    ) {
        let Some((remote, id, _)) = self.remote_parts(generation) else {
            return;
        };
        let (position_ms, playing) = match joined(answer) {
            Ok(released) if released.episode_id == id => (released.position_ms, released.playing),
            Ok(_) => (remote.position_ms(), remote.is_playing()),
            // Over already, so nothing plays on there to take over.
            Err(SonosError::Gone) => (remote.position_ms(), false),
            Err(error) => {
                let message = error.message(&remote.target.name);
                cx.emit(SonosNotice(message));
                return;
            }
        };
        // The store learns the released position before the next sync
        // brings the server's write of it, so a play from the list, or a
        // hand-off that reads the store, starts there.
        self.park(Some(position_ms));
        self.issue(vec![Write::Position {
            position_ms,
            while_playing: false,
        }]);
        match then {
            AfterRelease::Here if playing => {
                if let Some(current) = &self.current {
                    let mut episode = current.episode.clone();
                    episode.stored = Stored {
                        position_ms,
                        played: false,
                    };
                    self.start(episode, cx);
                }
            }
            AfterRelease::Here => {}
            AfterRelease::HandOff(target) => self.play_on(target, id, cx),
        }
        cx.notify();
    }

    // ==========================================================================
    // Steering the group
    // ==========================================================================

    /// Sends `command` to the group, showing its effect at once. If the
    /// group can't be reached, the session shows as it was, since the
    /// speaker never heard it.
    pub(super) fn command_remote(&mut self, command: SonosCommand, cx: &mut Context<Self>) {
        let Some(generation) = self.generation() else {
            return;
        };
        let Some((remote, _, duration_ms)) = self.remote_parts(generation) else {
            return;
        };
        if remote.is_connecting() {
            return;
        }
        let before = remote.command(command, duration_ms, Instant::now());
        let player_id = remote.target.id.clone();
        let api = self.sonos.clone();
        let answer = cx
            .global::<Background>()
            .spawn(async move { api.command(&player_id, command).await });
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                let Some((remote, id, _)) = this.remote_parts(generation) else {
                    return;
                };
                let now = Instant::now();
                match joined(answer) {
                    Ok(player) => {
                        if remote.answered(&player, id, now) == Verdict::Leave {
                            this.end_remote(generation, cx);
                        }
                    }
                    Err(SonosError::Gone) => this.end_remote(generation, cx),
                    Err(error) => {
                        remote.revert(before, now);
                        let message = error.message(&remote.target.name);
                        cx.emit(SonosNotice(message));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Sets the volume of the group the episode plays on, 0 to 100.
    pub fn set_volume(&mut self, volume: u8, cx: &mut Context<Self>) {
        let Some(generation) = self.generation() else {
            return;
        };
        let Some((remote, _, _)) = self.remote_parts(generation) else {
            return;
        };
        if remote.is_connecting() {
            return;
        }
        if let Some(volume) = remote.set_volume(volume, Instant::now()) {
            self.send_volume(generation, volume, cx);
        }
        cx.notify();
    }

    fn send_volume(&mut self, generation: u64, volume: u8, cx: &mut Context<Self>) {
        let Some((remote, _, _)) = self.remote_parts(generation) else {
            return;
        };
        let player_id = remote.target.id.clone();
        let api = self.sonos.clone();
        let answer = cx.global::<Background>().spawn(async move {
            api.command(&player_id, SonosCommand::Volume { volume })
                .await
        });
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                let Some((remote, _, _)) = this.remote_parts(generation) else {
                    return;
                };
                let next = match joined(answer) {
                    Ok(_) => remote.volume_sent(true),
                    Err(SonosError::Gone) => {
                        this.end_remote(generation, cx);
                        return;
                    }
                    Err(error) => {
                        let message = error.message(&remote.target.name);
                        let next = remote.volume_sent(false);
                        cx.emit(SonosNotice(message));
                        next
                    }
                };
                if let Some(next) = next {
                    this.send_volume(generation, next, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ==========================================================================
    // Following the session
    // ==========================================================================

    /// Runs the clock every [`TICK`] and polls the group when a poll is due:
    /// every 2 s while the window is active, every 15 s otherwise. One poll
    /// runs at a time. Ends when the player leaves this hand-off.
    fn follow(&self, generation: u64, cx: &mut Context<Self>) -> Task<()> {
        let background = cx.global::<Background>().clone();
        let api = self.sonos.clone();
        cx.spawn(async move |this, cx| {
            loop {
                // Timed on tokio, which keeps real time in UI tests too. The
                // sleep is made there: it needs the runtime's timer. A
                // runtime that shut down (the app quitting) ends the loop,
                // which would otherwise spin.
                let slept = background
                    .spawn(async { tokio::time::sleep(TICK).await })
                    .await;
                if slept.is_err() {
                    return;
                }
                let due = this.update(cx, |this, cx| {
                    let in_view = cx.active_window().is_some();
                    let (remote, _, duration_ms) = this.remote_parts(generation)?;
                    let now = Instant::now();
                    if remote.tick(now, duration_ms) {
                        cx.notify();
                    }
                    Some(remote.poll_due(now, in_view))
                });
                match due {
                    Ok(Some(true)) => {}
                    Ok(Some(false)) => continue,
                    Ok(None) | Err(_) => return,
                }
                let sent_at = Instant::now();
                let api = api.clone();
                let answer = background.spawn(async move { api.players().await }).await;
                let followed = this.update(cx, |this, cx| {
                    let players = match joined(answer) {
                        Ok(players) => players,
                        Err(error) => {
                            // A failed poll changes nothing; the next one
                            // tries again.
                            tracing::info!("polling the Sonos groups failed: {error}");
                            return;
                        }
                    };
                    this.sonos_available = !players.is_empty();
                    let Some((remote, id, _)) = this.remote_parts(generation) else {
                        return;
                    };
                    if remote.polled(&players, id, sent_at, Instant::now()) == Verdict::Leave {
                        this.end_remote(generation, cx);
                    }
                    cx.notify();
                });
                if followed.is_err() {
                    return;
                }
            }
        })
    }

    /// The session ended on the group's side: the episode finished, or the
    /// group plays something else now. The episode stays here, paused
    /// where it was last seen, and a sync brings the position and played
    /// state the server wrote.
    fn end_remote(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.remote_parts(generation).is_none() {
            return;
        }
        self.park(None);
        cx.global::<SyncHandle>().request_sync(Duration::ZERO);
        cx.notify();
    }

    // ==========================================================================
    // The phase
    // ==========================================================================

    /// Puts the current episode on `remote`, with the follow task running
    /// when `follow` (a hand-off waiting for its group has none yet).
    /// Returns the hand-off's generation.
    fn enter_remote(&mut self, remote: Remote, follow: bool, cx: &mut Context<Self>) -> u64 {
        self.outputs += 1;
        let generation = self.outputs;
        // A local play still loading from the store doesn't start.
        self.requests += 1;
        let follow = follow.then(|| self.follow(generation, cx));
        if let Some(current) = &mut self.current {
            current.phase = Phase::Remote {
                remote,
                generation,
                follow,
            };
        }
        cx.notify();
        generation
    }

    /// Leaves remote mode with the episode parked at `position_ms`, or
    /// where the session was last seen.
    fn park(&mut self, position_ms: Option<i64>) {
        let Some(current) = &mut self.current else {
            return;
        };
        let Some(last_seen_ms) = current.remote().map(Remote::position_ms) else {
            return;
        };
        current.parked_ms = position_ms.unwrap_or(last_seen_ms);
        current.phase = Phase::Parked;
    }

    fn generation(&self) -> Option<u64> {
        match &self.current.as_ref()?.phase {
            Phase::Remote { generation, .. } => Some(*generation),
            Phase::Parked | Phase::Loaded(_) | Phase::Failed(_) => None,
        }
    }

    /// The session of hand-off `generation`, with the episode's id and
    /// duration, while it is the player's.
    fn remote_parts(&mut self, generation: u64) -> Option<(&mut Remote, EpisodeId, Option<i64>)> {
        let current = self.current.as_mut()?;
        let (id, duration_ms) = (current.episode.id, current.duration_ms());
        match &mut current.phase {
            Phase::Remote {
                remote,
                generation: current,
                ..
            } if *current == generation => Some((remote, id, duration_ms)),
            _ => None,
        }
    }
}

/// A request's answer, with a request task that didn't finish (the
/// runtime shutting down) as a local failure.
fn joined<T>(answer: Result<Result<T, SonosError>, JoinError>) -> Result<T, SonosError> {
    answer.unwrap_or_else(|error| Err(SonosError::Local(error.into())))
}
