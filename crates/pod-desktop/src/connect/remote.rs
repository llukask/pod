//! Connect as the player sees it (spec 14.7), as pure functions and a pure
//! model: where the playback is relative to this device ([`role`]), what a
//! `command` event does to the local player ([`local_action`]), and the
//! playback of another device as the bar shows it ([`Remote`]). Playbacks
//! and commands come in with the instant they arrived; the player
//! (`player/output.rs`) sends the requests and owns the timers.
//!
//! The active device writes listening state, so nothing here produces a
//! write. What the bar shows between updates is a guess: the last position
//! plus the time since, times the speed, while playing.

use std::time::{Duration, Instant};

use pod_protocol::{ConnectCommand, DeviceId, EpisodeId, Playback};

/// How long after a volume change updates leave the slider alone. An
/// update the server sent before the change landed would otherwise jump it
/// back for a moment.
pub const VOLUME_HOLD: Duration = Duration::from_secs(3);

/// A device the playback can move to: a client device or a Sonos group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The device id, or the Sonos group's coordinator id.
    pub id: String,
    /// "Pixel 8", "Couch", or "Couch + Küche".
    pub name: String,
}

// ==============================================================================
// Decisions
// ==============================================================================

/// Where the account's playback is, from this device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role<'a> {
    /// The account has never played anything.
    Nothing,
    /// This device is the active one.
    Here(&'a Playback),
    /// Another client device or a Sonos group is: remote mode.
    Elsewhere(&'a Playback),
    /// Nothing is active, and the playback is the account's last one,
    /// paused.
    Idle(&'a Playback),
}

/// This device is a remote exactly when `playback.device_id` is set and
/// isn't `own` (spec 14.3).
pub fn role(playback: Option<&Playback>, own: DeviceId) -> Role<'_> {
    match playback {
        None => Role::Nothing,
        Some(playback) => match &playback.device_id {
            None => Role::Idle(playback),
            Some(_) if playback.is_active(own) => Role::Here(playback),
            Some(_) => Role::Elsewhere(playback),
        },
    }
}

/// What a `command` event does to the local player: what the button does
/// (spec 14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalAction {
    Pause,
    /// Resume the loaded play, or start the episode in the player by the
    /// start rule of spec 8.
    Resume,
    Seek(i64),
    /// Start another episode by the start rule, a replay included.
    Play(EpisodeId),
    /// Volume is for Sonos groups only.
    Ignore,
}

/// Maps `command` onto the local player standing at `position_ms`. A skip
/// moves from there, clamped to the episode (spec 14.4).
pub fn local_action(
    command: ConnectCommand,
    position_ms: i64,
    duration_ms: Option<i64>,
) -> LocalAction {
    let end = duration_ms.unwrap_or(i64::MAX).max(0);
    match command {
        ConnectCommand::Pause => LocalAction::Pause,
        ConnectCommand::Resume => LocalAction::Resume,
        ConnectCommand::Seek { position_ms } => LocalAction::Seek(position_ms.clamp(0, end)),
        ConnectCommand::Skip { delta_ms } => {
            LocalAction::Seek(position_ms.saturating_add(delta_ms).clamp(0, end))
        }
        ConnectCommand::Play { episode_id } => LocalAction::Play(episode_id),
        ConnectCommand::Volume { .. } => LocalAction::Ignore,
    }
}

/// Where a playing playback stands `elapsed` after it stood at
/// `position_ms`: it advances by the elapsed time times `speed` (spec 14.1).
pub fn interpolate(position_ms: i64, speed: f64, elapsed: Duration) -> i64 {
    let advanced = elapsed.as_secs_f64() * 1000.0 * speed;
    if !advanced.is_finite() || advanced <= 0.0 {
        return position_ms;
    }
    // `as` saturates at the bounds of i64.
    position_ms.saturating_add(advanced as i64)
}

// ==============================================================================
// Another device's playback, as shown
// ==============================================================================

/// The playback while another device is active, from the moment this
/// device sees it until it moves here or nothing is active any more.
#[derive(Debug)]
pub struct Remote {
    /// The active device: a client's device id or a Sonos player id.
    device_id: String,
    /// A Sonos group, which plays at 1x and has a volume.
    sonos: bool,
    playing: bool,
    speed: f64,
    /// The last known position and when it arrived, so the clock runs
    /// between updates.
    anchor_ms: i64,
    anchor_at: Instant,
    /// What the bar shows: the anchor, advanced while playing.
    shown_ms: i64,
    /// The group volume, while a Sonos group is active.
    volume: Option<u8>,
    volume_set_at: Option<Instant>,
    volume_queue: LatestOnly<u8>,
}

/// The playback as shown before a command, for [`Remote::revert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    playing: bool,
    position_ms: i64,
}

impl Remote {
    /// Follows `playback`, which arrived at `now`. Its `device_id` is set.
    pub fn follow(playback: &Playback, now: Instant) -> Remote {
        let mut remote = Remote {
            device_id: String::new(),
            sonos: false,
            playing: false,
            speed: 1.0,
            anchor_ms: 0,
            anchor_at: now,
            shown_ms: 0,
            volume: None,
            volume_set_at: None,
            volume_queue: LatestOnly::default(),
        };
        remote.update(playback, now);
        remote
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Whether a Sonos group is the active device. It plays at 1x only, and
    /// has a volume.
    pub fn is_sonos(&self) -> bool {
        self.sonos
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn position_ms(&self) -> i64 {
        self.shown_ms
    }

    /// The group volume, while a Sonos group is active.
    pub fn volume(&self) -> Option<u8> {
        self.volume
    }

    /// A newer playback of the same episode arrived at `now`. The volume
    /// stays as the user set it for [`VOLUME_HOLD`].
    pub fn update(&mut self, playback: &Playback, now: Instant) {
        self.device_id = playback.device_id.clone().unwrap_or_default();
        self.sonos = playback.client_device().is_none();
        self.speed = playback.speed;
        if self
            .volume_set_at
            .is_none_or(|at| now.saturating_duration_since(at) >= VOLUME_HOLD)
            || playback.volume.is_none()
        {
            self.volume = playback.volume;
        }
        self.show(playback.playing, playback.position_ms, now);
    }

    /// Runs the clock to `now`, at most to `duration_ms`. Returns whether
    /// the shown second changed, which is when the bar needs a new frame.
    pub fn tick(&mut self, now: Instant, duration_ms: Option<i64>) -> bool {
        if !self.playing {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.anchor_at);
        let shown = interpolate(self.anchor_ms, self.speed, elapsed)
            .min(duration_ms.unwrap_or(i64::MAX).max(self.anchor_ms));
        let changed = shown / 1000 != self.shown_ms / 1000;
        self.shown_ms = shown;
        changed
    }

    /// Shows what `command` will do before the active device has done it,
    /// and returns what showed before, for [`Remote::revert`]. Skips by
    /// `delta_ms` from the clock's guess, within `duration_ms`. `play` and
    /// `volume` show nothing here: the next playback brings the episode,
    /// and [`Remote::set_volume`] the volume.
    pub fn command(
        &mut self,
        command: ConnectCommand,
        duration_ms: Option<i64>,
        now: Instant,
    ) -> Shown {
        let before = Shown {
            playing: self.playing,
            position_ms: self.shown_ms,
        };
        let end = duration_ms.unwrap_or(i64::MAX).max(0);
        match command {
            ConnectCommand::Pause => self.show(false, self.shown_ms, now),
            ConnectCommand::Resume => self.show(true, self.shown_ms, now),
            ConnectCommand::Seek { position_ms } => {
                self.show(self.playing, position_ms.clamp(0, end), now);
            }
            ConnectCommand::Skip { delta_ms } => self.show(
                self.playing,
                self.shown_ms.saturating_add(delta_ms).clamp(0, end),
                now,
            ),
            ConnectCommand::Play { .. } | ConnectCommand::Volume { .. } => {}
        }
        before
    }

    /// The command failed, so the device never heard it: shows the
    /// playback as it was.
    pub fn revert(&mut self, before: Shown, now: Instant) {
        self.show(before.playing, before.position_ms, now);
    }

    /// Shows the volume the user set, and returns it when it is to be sent
    /// now. While one volume command is out, only the newest value waits.
    pub fn set_volume(&mut self, volume: u8, now: Instant) -> Option<u8> {
        let volume = volume.min(100);
        self.volume = Some(volume);
        self.volume_set_at = Some(now);
        self.volume_queue.push(volume)
    }

    /// The volume command came back, `ok` or not. Returns the value that
    /// waited meanwhile, which goes out next.
    pub fn volume_sent(&mut self, ok: bool) -> Option<u8> {
        if !ok {
            // The next update shows the volume the group really has.
            self.volume_set_at = None;
        }
        self.volume_queue.finished()
    }

    fn show(&mut self, playing: bool, position_ms: i64, now: Instant) {
        self.playing = playing;
        self.anchor_ms = position_ms;
        self.anchor_at = now;
        self.shown_ms = position_ms;
    }
}

// ==============================================================================
// Latest value only
// ==============================================================================

/// Sends values one at a time, and while one is out only the newest waits.
/// A dragged slider then sends the value the user ends on, without a
/// request for every step.
#[derive(Debug)]
pub struct LatestOnly<T> {
    busy: bool,
    pending: Option<T>,
}

impl<T> Default for LatestOnly<T> {
    fn default() -> Self {
        LatestOnly {
            busy: false,
            pending: None,
        }
    }
}

impl<T> LatestOnly<T> {
    /// Queues `value`. Returns it when nothing is out, to be sent now.
    pub fn push(&mut self, value: T) -> Option<T> {
        if self.busy {
            self.pending = Some(value);
            None
        } else {
            self.busy = true;
            Some(value)
        }
    }

    /// The value out came back. Returns the one to send next, if any.
    pub fn finished(&mut self) -> Option<T> {
        let next = self.pending.take();
        self.busy = next.is_some();
        next
    }
}

#[cfg(test)]
mod tests {
    use pod_protocol::PlaybackEpisode;

    use super::*;

    const EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c01";
    const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
    const OWN: &str = "019a3f10-8c2e-7a41-9b3d-5e6f7a8b9c0d";
    const PHONE: &str = "019a3f10-8c2e-7a41-9b3d-000000000001";
    const COUCH: &str = "RINCON_804AF246458401400";

    fn own() -> DeviceId {
        OWN.parse().expect("the id is valid")
    }

    fn episode() -> EpisodeId {
        EPISODE.parse().expect("the id is valid")
    }

    fn playback(device_id: Option<&str>, playing: bool, position_ms: i64) -> Playback {
        Playback {
            device_id: device_id.map(str::to_owned),
            episode: PlaybackEpisode {
                id: episode(),
                podcast_id: PODCAST.parse().expect("the id is valid"),
                title: "EP 151: The pager".to_owned(),
                podcast_title: Some("Darknet Diaries".to_owned()),
                artwork_url: None,
                duration_ms: Some(3_120_000),
            },
            position_ms,
            playing,
            speed: 1.0,
            volume: device_id
                .is_some_and(|id| id.starts_with("RINCON_"))
                .then_some(12),
        }
    }

    #[test]
    fn this_device_is_a_remote_exactly_while_another_is_active() {
        assert_eq!(role(None, own()), Role::Nothing);
        let here = playback(Some(OWN), true, 0);
        assert_eq!(role(Some(&here), own()), Role::Here(&here));
        let phone = playback(Some(PHONE), true, 0);
        assert_eq!(role(Some(&phone), own()), Role::Elsewhere(&phone));
        let couch = playback(Some(COUCH), true, 0);
        assert_eq!(role(Some(&couch), own()), Role::Elsewhere(&couch));
        let idle = playback(None, false, 0);
        assert_eq!(role(Some(&idle), own()), Role::Idle(&idle));
    }

    #[test]
    fn commands_map_onto_the_local_buttons() {
        let at = |command| local_action(command, 60_000, Some(100_000));
        assert_eq!(at(ConnectCommand::Pause), LocalAction::Pause);
        assert_eq!(at(ConnectCommand::Resume), LocalAction::Resume);
        assert_eq!(
            at(ConnectCommand::Skip { delta_ms: -15_000 }),
            LocalAction::Seek(45_000)
        );
        assert_eq!(
            at(ConnectCommand::Skip { delta_ms: -90_000 }),
            LocalAction::Seek(0),
            "clamped at the start"
        );
        assert_eq!(
            at(ConnectCommand::Skip { delta_ms: 60_000 }),
            LocalAction::Seek(100_000),
            "clamped at the end"
        );
        assert_eq!(
            at(ConnectCommand::Seek { position_ms: 5_000 }),
            LocalAction::Seek(5_000)
        );
        assert_eq!(
            at(ConnectCommand::Play {
                episode_id: episode()
            }),
            LocalAction::Play(episode())
        );
        assert_eq!(
            at(ConnectCommand::Volume { volume: 30 }),
            LocalAction::Ignore
        );
        assert_eq!(
            local_action(ConnectCommand::Skip { delta_ms: 30_000 }, 60_000, None),
            LocalAction::Seek(90_000),
            "no end without a duration"
        );
    }

    #[test]
    fn the_position_advances_by_the_elapsed_time_times_the_speed() {
        assert_eq!(
            interpolate(10_000, 1.0, Duration::from_millis(1_500)),
            11_500
        );
        assert_eq!(interpolate(10_000, 1.5, Duration::from_secs(2)), 13_000);
        assert_eq!(interpolate(10_000, 2.0, Duration::ZERO), 10_000);
        assert_eq!(
            interpolate(i64::MAX - 1, 2.0, Duration::from_secs(60)),
            i64::MAX
        );
    }

    #[test]
    fn the_clock_runs_while_playing_at_the_playback_speed() {
        let start = Instant::now();
        let mut fast = playback(Some(PHONE), true, 60_000);
        fast.speed = 2.0;
        let mut remote = Remote::follow(&fast, start);
        assert!(!remote.is_sonos());
        assert!(!remote.tick(start + Duration::from_millis(400), None));
        assert!(remote.tick(start + Duration::from_millis(500), None));
        assert_eq!(remote.position_ms(), 61_000);
        assert!(remote.tick(start + Duration::from_secs(10), Some(65_000)));
        assert_eq!(remote.position_ms(), 65_000, "never past the end");

        remote.update(
            &playback(Some(PHONE), false, 70_000),
            start + Duration::from_secs(10),
        );
        assert!(!remote.tick(start + Duration::from_secs(20), None));
        assert_eq!(
            remote.position_ms(),
            70_000,
            "a paused playback stands still"
        );
    }

    #[test]
    fn commands_show_at_once_and_revert_when_unheard() {
        let start = Instant::now();
        let mut remote = Remote::follow(&playback(Some(PHONE), true, 60_000), start);
        let before = remote.command(ConnectCommand::Skip { delta_ms: -90_000 }, None, start);
        assert_eq!(remote.position_ms(), 0, "clamped at the start");
        remote.command(
            ConnectCommand::Skip { delta_ms: 30_000 },
            Some(20_000),
            start,
        );
        assert_eq!(remote.position_ms(), 20_000, "clamped at the end");
        remote.command(ConnectCommand::Pause, None, start);
        assert!(!remote.is_playing());
        remote.command(
            ConnectCommand::Play {
                episode_id: episode(),
            },
            None,
            start,
        );
        assert_eq!(
            (remote.position_ms(), remote.is_playing()),
            (20_000, false),
            "another episode shows once it plays"
        );
        remote.revert(before, start);
        assert_eq!((remote.position_ms(), remote.is_playing()), (60_000, true));
    }

    #[test]
    fn a_sonos_group_has_a_volume_that_updates_wait_for_after_a_change() {
        let start = Instant::now();
        let mut remote = Remote::follow(&playback(Some(COUCH), true, 0), start);
        assert!(remote.is_sonos());
        assert_eq!(remote.volume(), Some(12));
        assert_eq!(remote.set_volume(40, start), Some(40));
        let soon = start + Duration::from_secs(1);
        remote.update(&playback(Some(COUCH), true, 1_000), soon);
        assert_eq!(remote.volume(), Some(40), "the update's 12 is ignored");
        let later = start + VOLUME_HOLD;
        remote.update(&playback(Some(COUCH), true, 3_000), later);
        assert_eq!(remote.volume(), Some(12));
    }

    #[test]
    fn only_the_newest_volume_waits_while_one_is_out() {
        let start = Instant::now();
        let mut remote = Remote::follow(&playback(Some(COUCH), true, 0), start);
        assert_eq!(remote.set_volume(20, start), Some(20));
        assert_eq!(remote.set_volume(25, start), None);
        assert_eq!(remote.set_volume(31, start), None);
        assert_eq!(remote.volume(), Some(31));
        assert_eq!(remote.volume_sent(true), Some(31));
        assert_eq!(remote.volume_sent(true), None);
        assert_eq!(remote.set_volume(40, start), Some(40), "idle again");
    }
}
