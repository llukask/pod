//! Where the episode plays (spec 14): this device's part in Connect.
//!
//! The player holds the account's event stream ([`stream`]) while it has
//! an account, and with a connection open it reports the local play
//! (14.3). The stream's events run here: a `command` as if its button was
//! pressed, a `load` as a play from that position, a `release` as a pause.
//!
//! While another device is active, the player's phase is
//! [`Phase::Remote`]: mpv is stopped, the contract is gone and nothing here
//! writes listening state. The buttons send commands to the active device
//! instead, and a clock task advances the shown position between updates.
//! The pure decisions are [`crate::connect::remote`]; this module sends the
//! requests (on the tokio side, never on the UI thread) and applies their
//! answers.
//!
//! Each remote mode gets a generation. A command's answer for an older
//! one, which the player has since left, changes nothing. Likewise an
//! answer that a `state` event overtook changes nothing: the event is
//! newer.

use std::time::{Duration, Instant};

use gpui_kit::{Context, Task};
use pod_protocol::{
    ConnectCommand, ConnectDevice, ConnectEvent, ConnectReport, EpisodeId, Playback,
    PlaybackEpisode, SonosPlayer,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinError, JoinHandle};
use uuid::Uuid;

use super::{ConnectNotice, Current, Episode, Job, Phase, Player, Source};
use crate::connect::api::Commanded;
use crate::connect::remote::{LocalAction, Role, local_action, role};
use crate::connect::stream::{self, Link, Signal};
use crate::connect::{self, ConnectError, Remote, Target};
use crate::runtime::Background;
use crate::sync::SyncHandle;

/// How often the clock task advances the shown position. The bar shows
/// whole seconds.
const TICK: Duration = Duration::from_millis(500);

/// How often the player reports while it plays (spec 14.3).
pub const HEARTBEAT: Duration = Duration::from_secs(15);

/// What a remote mode is called when nothing names its device.
const SOMEWHERE: &str = "another device";

/// A report, and where its answer goes.
pub(super) type Report = (
    ConnectReport,
    oneshot::Sender<Result<Playback, ConnectError>>,
);

/// Sends reports one at a time, so the server hears them in the order the
/// player made them.
pub(super) async fn send_reports(api: connect::Api, mut reports: mpsc::UnboundedReceiver<Report>) {
    while let Some((report, reply)) = reports.recv().await {
        let _ = reply.send(api.report(report).await);
    }
}

/// Reports every [`HEARTBEAT`] while the episode plays here. Timed on
/// tokio, which keeps real time in UI tests too.
pub(super) fn heartbeat(background: Background, cx: &mut Context<Player>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            // A runtime that shut down (the app quitting) ends the loop.
            let slept = background
                .spawn(async { tokio::time::sleep(HEARTBEAT).await })
                .await;
            if slept.is_err() {
                return;
            }
            let reported = this.update(cx, |this, cx| {
                if this.is_playing_here() {
                    this.report(None, cx);
                }
            });
            if reported.is_err() {
                return;
            }
        }
    })
}

/// The event stream's task and the one that hands its signals to the
/// player. Dropping it closes the stream.
#[derive(Debug)]
pub(super) struct Stream {
    task: JoinHandle<()>,
    _signals: Task<()>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Player {
    // ==========================================================================
    // What the picker and the bar read
    // ==========================================================================

    /// Whether the output picker has anything to offer besides this device:
    /// another connected device or a Sonos group, or a playback elsewhere
    /// to take back.
    pub fn outputs_available(&self) -> bool {
        !self.other_devices().is_empty()
            || !self.sonos_players.is_empty()
            || self.is_remote()
            || self.transfer.is_some()
    }

    /// The account's other connected client devices, in connection order.
    pub fn other_devices(&self) -> Vec<ConnectDevice> {
        let own = self.link.as_ref().map(|link| link.device_id);
        self.devices
            .iter()
            .filter(|device| Some(device.id) != own)
            .cloned()
            .collect()
    }

    /// This device's id, while signed in.
    pub fn own_device(&self) -> Option<String> {
        self.link.as_ref().map(|link| link.device_id.to_string())
    }

    /// The device the episode plays on, `None` for this one.
    pub fn active_output(&self) -> Option<&str> {
        self.current.as_ref()?.remote().map(Remote::device_id)
    }

    /// The name of the device or Sonos group `id`.
    pub fn output_name(&self, id: &str) -> String {
        self.devices
            .iter()
            .find(|device| device.id.to_string() == id)
            .map(|device| device.name.clone())
            .or_else(|| {
                self.sonos_players
                    .iter()
                    .find(|player| player.id == id)
                    .map(|player| player.name.clone())
            })
            .unwrap_or_else(|| SOMEWHERE.to_owned())
    }

    /// Whether the stream's connection delivered its first `state`, so the
    /// player reports.
    #[cfg(test)]
    pub fn connected(&self) -> bool {
        self.stream_open
    }

    /// The transfer waiting for its answer, if one is.
    pub fn transferring(&self) -> Option<&Target> {
        self.transfer.as_ref()
    }

    pub(super) fn is_remote(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|current| current.remote().is_some())
    }

    /// Lists the Sonos groups for the output picker, empty when there are
    /// none or the server can't be asked. Keeps the cached list current.
    pub fn list_players(&mut self, cx: &mut Context<Self>) -> Task<Vec<SonosPlayer>> {
        let api = self.connect.clone();
        let listed = cx
            .global::<Background>()
            .spawn(async move { api.players().await });
        cx.spawn(async move |this, cx| {
            let players = joined(listed.await).unwrap_or_else(|error| {
                tracing::info!("listing the Sonos groups failed: {error}");
                Vec::new()
            });
            let _ = this.update(cx, |this, cx| {
                this.sonos_players = players.clone();
                cx.notify();
            });
            players
        })
    }

    // ==========================================================================
    // The event stream
    // ==========================================================================

    /// The account changed: closes the old account's stream and opens one
    /// for `link`, and asks whether the account has Sonos groups.
    pub(super) fn set_link(&mut self, link: Option<Link>, cx: &mut Context<Self>) {
        if link == self.link {
            return;
        }
        self.stream = None;
        self.stream_closed(cx);
        self.sonos_players.clear();
        self.playback = None;
        self.link = link.clone();
        if let Some(link) = link {
            self.stream = Some(self.open_stream(link, cx));
            self.list_players(cx).detach();
        }
    }

    fn open_stream(&self, link: Link, cx: &mut Context<Self>) -> Stream {
        let (signals, mut received) = mpsc::unbounded_channel();
        let task =
            cx.global::<Background>()
                .spawn(stream::run(self.stream_http.clone(), link, signals));
        let signals = cx.spawn(async move |this, cx| {
            while let Some(signal) = received.recv().await {
                if this
                    .update(cx, |this, cx| this.on_signal(signal, cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        Stream {
            task,
            _signals: signals,
        }
    }

    fn on_signal(&mut self, signal: Signal, cx: &mut Context<Self>) {
        match signal {
            Signal::Event(event) => self.on_connect_event(event, cx),
            Signal::Closed => self.stream_closed(cx),
            Signal::SignedOut => {
                // The sync engine finds the same and shows the device as
                // signed out (spec 5.7). Signing in again changes the
                // account, which opens a new stream.
                self.stream_closed(cx);
                self.request_sync(cx);
            }
            Signal::Replaced => {
                tracing::warn!("another instance speaks for this device now");
                self.stream_closed(cx);
            }
        }
        cx.notify();
    }

    /// Without a stream, Connect is off and this device plays locally
    /// (spec 14). A remote mode ends where the playback was last seen, and
    /// a sync brings what the active device wrote.
    fn stream_closed(&mut self, cx: &mut Context<Self>) {
        self.stream_open = false;
        self.devices.clear();
        if self.is_remote() {
            self.leave_remote(None, cx);
            self.request_sync(cx);
        }
    }

    fn on_connect_event(&mut self, event: ConnectEvent, cx: &mut Context<Self>) {
        match event {
            ConnectEvent::State(state) => {
                let first = !self.stream_open;
                self.stream_open = true;
                self.states += 1;
                self.devices = state.devices;
                self.follow_playback(state.playback, first, cx);
            }
            ConnectEvent::Command { command } => self.run_command(command, cx),
            ConnectEvent::Load {
                episode_id,
                position_ms,
            } => self.load_here(episode_id, position_ms, cx),
            ConnectEvent::Release { transfer_id } => self.release_here(transfer_id, cx),
            // The stream task ends with `Signal::Replaced`.
            ConnectEvent::Replaced => {}
        }
    }

    /// Shows the account's `playback`. `first` is the first one of a
    /// connection, which reports a local play, so a device that comes back
    /// while playing claims the playback again (spec 14.1).
    fn follow_playback(&mut self, playback: Option<Playback>, first: bool, cx: &mut Context<Self>) {
        let Some(own) = self.link.as_ref().map(|link| link.device_id) else {
            return;
        };
        self.playback = playback.clone();
        let was_remote = self.is_remote();
        let playing_here = self.is_playing_here();
        match role(playback.as_ref(), own) {
            Role::Nothing => {
                if was_remote {
                    self.leave_remote(None, cx);
                    self.request_sync(cx);
                }
            }
            Role::Here(playback) => {
                // Moving here: the `load` that comes with it starts it.
                if was_remote {
                    self.leave_remote(Some(playback), cx);
                }
                // The server still thinks it plays here, as after a restart
                // within the grace period: the report sets it right.
                if first && playback.playing && !playing_here {
                    self.report(None, cx);
                }
            }
            // Playing here wins until a `release` says otherwise: a state
            // sent before this device's last report doesn't know about it.
            Role::Elsewhere(playback) if !self.is_busy_here() => self.follow_remote(playback, cx),
            Role::Elsewhere(_) => {}
            Role::Idle(playback) => {
                if was_remote {
                    // The remote ended (spec 14.7).
                    self.leave_remote(Some(playback), cx);
                    self.request_sync(cx);
                } else if !self.is_busy_here() {
                    self.show_last(playback, cx);
                }
            }
        }
        if first && playing_here {
            self.report(None, cx);
        }
        cx.notify();
    }

    /// Nothing is active: shows the account's last playback in place of the
    /// episode in the player, unless that is the same episode, whose local
    /// position stands (spec 14.7).
    fn show_last(&mut self, playback: &Playback, cx: &mut Context<Self>) {
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.episode.id == playback.episode.id)
        {
            return;
        }
        if self.session_mut().is_some() {
            self.send(super::Command::Stop);
        }
        self.end_session(super::contract::Contract::stop);
        self.show_episode(
            &playback.episode,
            Phase::Parked,
            playback.position_ms,
            true,
            cx,
        );
    }

    /// Puts `episode` in the player with `phase` at once, from the
    /// playback's metadata, and swaps in the store's row once it is read,
    /// which has the cached artwork. With `local_position`, a parked
    /// episode then stands at its local start position, so a position this
    /// device hasn't synced yet still counts.
    pub(super) fn show_episode(
        &mut self,
        episode: &PlaybackEpisode,
        phase: Phase,
        parked_ms: i64,
        local_position: bool,
        cx: &mut Context<Self>,
    ) {
        self.current = Some(Current::new(
            Episode::from_playback(episode),
            parked_ms,
            phase,
        ));
        let id = episode.id;
        let loaded = self.load(Some(id));
        cx.spawn(async move |this, cx| {
            let Ok(Some(local)) = loaded.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let Some(current) = &mut this.current else {
                    return;
                };
                if current.episode.id != id || current.episode.source != Source::Elsewhere {
                    return;
                }
                if local_position && matches!(current.phase, Phase::Parked) {
                    current.parked_ms = local.stored.start_position_ms();
                }
                current.episode = local;
                cx.notify();
            });
        })
        .detach();
    }

    // ==========================================================================
    // Events for the active device
    // ==========================================================================

    /// Runs `command` from another device as if its button was pressed
    /// here, so the writes of spec 8 follow (14.2).
    fn run_command(&mut self, command: ConnectCommand, cx: &mut Context<Self>) {
        // The server sends commands to the active device only.
        if self.is_remote() {
            self.leave_remote(None, cx);
        }
        let (position_ms, duration_ms) = self.current.as_ref().map_or((0, None), |current| {
            (current.position_ms(), current.duration_ms())
        });
        match local_action(command, position_ms, duration_ms) {
            LocalAction::Pause => {
                if self.pause_local() {
                    self.report(None, cx);
                }
            }
            LocalAction::Resume => self.resume_or_start(cx),
            LocalAction::Seek(position_ms) => {
                self.seek_local(position_ms);
                self.report(None, cx);
            }
            LocalAction::Play(id) => self.play_here(id, None, cx),
            LocalAction::Ignore => {}
        }
        cx.notify();
    }

    /// A transfer moves the playback here: plays `id` from `position_ms`,
    /// which isn't a replay, or by the start rule over local state when it
    /// is `None` (spec 14.2). Starting reports.
    fn load_here(&mut self, id: EpisodeId, position_ms: Option<i64>, cx: &mut Context<Self>) {
        if self.is_remote() {
            self.leave_remote(None, cx);
        }
        self.play_here(id, position_ms, cx);
        cx.notify();
    }

    /// This device stops being the active one: pauses (an ordinary pause
    /// write) and reports with `released` (spec 14.2). The next playback
    /// says which device follows.
    fn release_here(&mut self, transfer_id: Option<Uuid>, cx: &mut Context<Self>) {
        // A play still reading its episode doesn't start.
        if self.loading.take().is_some() {
            self.requests += 1;
        }
        self.pause_local();
        self.report(transfer_id, cx);
        cx.notify();
    }

    // ==========================================================================
    // Reports
    // ==========================================================================

    /// Reports the local player while the stream is open (spec 14.3). The
    /// answer says whether another device is active.
    pub(super) fn report(&mut self, released: Option<Uuid>, cx: &mut Context<Self>) {
        if !self.stream_open {
            return;
        }
        let Some(current) = &self.current else {
            return;
        };
        if current.remote().is_some() {
            return;
        }
        let report = ConnectReport {
            episode_id: current.episode.id,
            position_ms: current.position_ms().max(0),
            playing: current.is_playing(),
            speed: self.settings.speed,
            released,
        };
        let (reply, answer) = oneshot::channel();
        if self.reports.send((report, reply)).is_err() {
            return;
        }
        let states = self.states;
        cx.spawn(async move |this, cx| {
            let Ok(answer) = answer.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| match answer {
                Ok(playback) => this.answered(states, playback, cx),
                // The stream closed meanwhile; it reports again once open.
                Err(ConnectError::NoSession) => {}
                Err(ConnectError::SignedOut) => this.request_sync(cx),
                Err(error) => tracing::info!("the report failed: {error}"),
            });
        })
        .detach();
    }

    /// An answer with the playback, from a request sent while `states`
    /// state events had arrived. One that arrived since is newer.
    fn answered(&mut self, states: u64, playback: Playback, cx: &mut Context<Self>) {
        if states == self.states && self.stream_open {
            self.follow_playback(Some(playback), false, cx);
        }
    }

    // ==========================================================================
    // Transfers
    // ==========================================================================

    /// Moves the playback to `target` (spec 14.5). From this device, the
    /// server releases the local play first. With nothing active, the
    /// outbox is pushed first, since the server then starts from its own
    /// copy of the listening state.
    pub fn transfer(&mut self, target: Target, cx: &mut Context<Self>) {
        if self.transfer.is_some() {
            return;
        }
        let here = self.own_device().as_deref() == Some(target.id.as_str());
        let nothing_active = self
            .playback
            .as_ref()
            .is_none_or(|playback| playback.device_id.is_none());
        let push = nothing_active && !here;
        let (reply, written) = oneshot::channel();
        let _ = self.jobs.send(Job::Barrier(reply));
        let (engine, api, device_id) =
            (self.engine.clone(), self.connect.clone(), target.id.clone());
        let answer = cx.global::<Background>().spawn(async move {
            if push {
                let _ = written.await;
                // A failed push leaves the writes queued for the next sync.
                match engine.sync().await {
                    Ok(state) => tracing::debug!(?state, "pushed the outbox before a transfer"),
                    Err(error) => tracing::warn!("pushing before a transfer failed: {error:#}"),
                }
            }
            api.transfer(device_id).await
        });
        self.transfer = Some(target.clone());
        let states = self.states;
        cx.spawn(async move |this, cx| {
            let answer = joined(answer.await);
            let _ = this.update(cx, |this, cx| {
                this.transfer = None;
                match answer {
                    Ok(playback) => this.answered(states, playback, cx),
                    Err(error) => cx.emit(ConnectNotice(error.message(&target.name))),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ==========================================================================
    // Remote mode
    // ==========================================================================

    /// Follows `playback` of another device: stops mpv, which writes
    /// nothing more (the release's pause saved the position), and shows
    /// the playback with its clock running.
    fn follow_remote(&mut self, playback: &Playback, cx: &mut Context<Self>) {
        let now = Instant::now();
        if let Some(current) = &mut self.current
            && current.episode.id == playback.episode.id
            && let Phase::Remote { remote, .. } = &mut current.phase
        {
            remote.update(playback, now);
            return;
        }
        if self.session_mut().is_some() {
            self.send(super::Command::Stop);
            self.reconciler.set_playing(None);
        }
        // A local play still loading from the store doesn't start.
        self.requests += 1;
        self.loading = None;
        self.outputs += 1;
        let generation = self.outputs;
        let phase = Phase::Remote {
            remote: Remote::follow(playback, now),
            generation,
            _clock: self.clock(generation, cx),
        };
        match self
            .current
            .as_mut()
            .filter(|current| current.episode.id == playback.episode.id)
        {
            Some(current) => current.phase = phase,
            None => self.show_episode(&playback.episode, phase, playback.position_ms, false, cx),
        }
        // A group the list doesn't name yet, such as one started from
        // another device.
        if let Some(id) = &playback.device_id
            && playback.client_device().is_none()
            && !self.sonos_players.iter().any(|player| &player.id == id)
        {
            self.list_players(cx).detach();
        }
    }

    /// Leaves remote mode with the episode parked: on `playback`'s episode
    /// and position, or where the remote was last seen.
    fn leave_remote(&mut self, playback: Option<&Playback>, cx: &mut Context<Self>) {
        let Some(current) = &mut self.current else {
            return;
        };
        let Some(last_seen_ms) = current.remote().map(Remote::position_ms) else {
            return;
        };
        match playback {
            Some(playback) if playback.episode.id != current.episode.id => {
                self.show_episode(
                    &playback.episode,
                    Phase::Parked,
                    playback.position_ms,
                    false,
                    cx,
                );
            }
            Some(playback) => {
                current.parked_ms = playback.position_ms;
                current.phase = Phase::Parked;
            }
            None => {
                current.parked_ms = last_seen_ms;
                current.phase = Phase::Parked;
            }
        }
    }

    /// Sends `command` to the active device, showing its effect at once. If
    /// it fails, the playback shows as it was, since the device never
    /// heard it.
    pub(super) fn command_remote(&mut self, command: ConnectCommand, cx: &mut Context<Self>) {
        let Some(generation) = self.generation() else {
            return;
        };
        let Some((remote, _, duration_ms)) = self.remote_parts(generation) else {
            return;
        };
        let now = Instant::now();
        let before = remote.command(command, duration_ms, now);
        let api = self.connect.clone();
        let answer = cx
            .global::<Background>()
            .spawn(async move { api.command(command).await });
        let states = self.states;
        cx.spawn(async move |this, cx| {
            let answer = joined(answer.await);
            let _ = this.update(cx, |this, cx| {
                match answer {
                    // The device runs it and its next report shows it.
                    Ok(Commanded { relayed: true, .. }) => {}
                    Ok(Commanded { playback, .. }) => this.answered(states, playback, cx),
                    Err(ConnectError::NoSession) => this.remote_ended(generation, cx),
                    Err(error) => {
                        let Some((remote, _, _)) = this.remote_parts(generation) else {
                            return;
                        };
                        remote.revert(before, Instant::now());
                        let device = this.remote_device(generation);
                        let name = this.output_name(&device);
                        cx.emit(ConnectNotice(error.message(&name)));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Sets the volume of the active Sonos group, 0 to 100.
    pub fn set_volume(&mut self, volume: u8, cx: &mut Context<Self>) {
        let Some(generation) = self.generation() else {
            return;
        };
        let Some((remote, _, _)) = self.remote_parts(generation) else {
            return;
        };
        if !remote.is_sonos() {
            return;
        }
        if let Some(volume) = remote.set_volume(volume, Instant::now()) {
            self.send_volume(generation, volume, cx);
        }
        cx.notify();
    }

    fn send_volume(&mut self, generation: u64, volume: u8, cx: &mut Context<Self>) {
        let api = self.connect.clone();
        let answer = cx
            .global::<Background>()
            .spawn(async move { api.command(ConnectCommand::Volume { volume }).await });
        cx.spawn(async move |this, cx| {
            let answer = joined(answer.await);
            let _ = this.update(cx, |this, cx| {
                let Some((remote, _, _)) = this.remote_parts(generation) else {
                    return;
                };
                let next = match answer {
                    Ok(_) => remote.volume_sent(true),
                    Err(ConnectError::NoSession) => {
                        this.remote_ended(generation, cx);
                        return;
                    }
                    Err(error) => {
                        let next = remote.volume_sent(false);
                        let device = this.remote_device(generation);
                        let name = this.output_name(&device);
                        cx.emit(ConnectNotice(error.message(&name)));
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

    /// A command found nothing active: the remote ended before its `state`
    /// said so.
    fn remote_ended(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.remote_parts(generation).is_none() {
            return;
        }
        self.leave_remote(None, cx);
        self.request_sync(cx);
        cx.notify();
    }

    /// Advances the shown position every [`TICK`] while generation
    /// `generation` is the player's remote mode.
    fn clock(&self, generation: u64, cx: &mut Context<Self>) -> Task<()> {
        let background = cx.global::<Background>().clone();
        cx.spawn(async move |this, cx| {
            loop {
                // Timed on tokio, which keeps real time in UI tests too. A
                // runtime that shut down (the app quitting) ends the loop,
                // which would otherwise spin.
                let slept = background
                    .spawn(async { tokio::time::sleep(TICK).await })
                    .await;
                if slept.is_err() {
                    return;
                }
                let ticked = this.update(cx, |this, cx| {
                    let (remote, _, duration_ms) = this.remote_parts(generation)?;
                    if remote.tick(Instant::now(), duration_ms) {
                        cx.notify();
                    }
                    Some(())
                });
                if !matches!(ticked, Ok(Some(()))) {
                    return;
                }
            }
        })
    }

    fn request_sync(&self, cx: &mut Context<Self>) {
        cx.global::<SyncHandle>().request_sync(Duration::ZERO);
    }

    fn generation(&self) -> Option<u64> {
        match &self.current.as_ref()?.phase {
            Phase::Remote { generation, .. } => Some(*generation),
            Phase::Parked | Phase::Loaded(_) | Phase::Failed(_) => None,
        }
    }

    /// The active device of remote mode `generation`, or nothing.
    fn remote_device(&mut self, generation: u64) -> String {
        self.remote_parts(generation)
            .map(|(remote, _, _)| remote.device_id().to_owned())
            .unwrap_or_default()
    }

    /// The remote of mode `generation`, with the episode's id and duration,
    /// while it is the player's.
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
fn joined<T>(answer: Result<Result<T, ConnectError>, JoinError>) -> Result<T, ConnectError> {
    answer.unwrap_or_else(|error| Err(ConnectError::Local(error.into())))
}
