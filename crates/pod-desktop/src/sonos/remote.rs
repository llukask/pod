//! A session on a Sonos group as the player shows it (spec 13.4), as a
//! pure model: the polls, command answers and the clock come in with the
//! instant they happened, and the model says what the player bar shows and
//! whether the session is still there. The player (`player/output.rs`)
//! sends the requests and owns the timers.
//!
//! pod-server plays the episode and writes its listening state, so nothing
//! here produces a write. What the bar shows between polls is a guess: the
//! last observed position plus the time since, while playing.

use std::time::{Duration, Instant};

use pod_protocol::{EpisodeId, SonosCommand, SonosPlayer, SonosPlayerState, SonosSessionView};

use crate::player::contract::Stored;

/// How often the session is polled while the window is active, and while
/// it isn't (spec 13.4).
pub const POLL_IN_VIEW: Duration = Duration::from_secs(2);
pub const POLL_AWAY: Duration = Duration::from_secs(15);

/// How long the group may be missing from the list before its session
/// counts as over. The server leaves out a group that didn't answer this
/// time, and keeps the session of one that stays silent this long (spec
/// 13.3).
pub const MISSING_LIMIT: Duration = Duration::from_secs(2 * 60);

/// How long after a volume change polls leave the slider alone. A poll sent
/// before the change landed would otherwise jump it back for a moment.
pub const VOLUME_HOLD: Duration = Duration::from_secs(3);

/// A Sonos group the episode can play on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The coordinator's UUID, which the API addresses the group by.
    pub id: String,
    /// "Couch", or "Couch + Küche".
    pub name: String,
}

impl From<&SonosPlayer> for Target {
    fn from(player: &SonosPlayer) -> Self {
        Target {
            id: player.id.clone(),
            name: player.name.clone(),
        }
    }
}

// ==============================================================================
// Decisions
// ==============================================================================

/// Where a hand-off starts, by the start rule of spec 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Start {
    pub position_ms: i64,
    /// A played episode starts over, and writes `played = false` first, as
    /// a local start would.
    pub unplay: bool,
}

pub fn hand_off_start(stored: Stored) -> Start {
    Start {
        position_ms: stored.start_position_ms(),
        unplay: stored.played,
    }
}

/// What a player list says about the session of `episode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen<'a> {
    /// The user's session still holds the episode on `player`.
    Session(&'a SonosPlayer, SonosSessionView),
    /// The session is over: the episode ended, or the group plays something
    /// else now.
    Ended,
    /// The group is missing from the list, which the server does for a
    /// group that didn't answer this time.
    Missing,
}

/// Reads the session of `episode` out of `players`: on `player_id`, or on
/// any group when that is `None` (picking a session up on start). A session
/// of another episode counts as ended, since the player has nothing to show
/// for it.
pub fn observe<'a>(
    players: &'a [SonosPlayer],
    episode: EpisodeId,
    player_id: Option<&str>,
) -> Seen<'a> {
    let mut candidates = players
        .iter()
        .filter(|player| player_id.is_none_or(|id| player.id == id))
        .peekable();
    if player_id.is_some() && candidates.peek().is_none() {
        return Seen::Missing;
    }
    candidates
        .find_map(|player| match player.session {
            Some(session)
                if player.state == SonosPlayerState::Pod && session.episode_id == episode =>
            {
                Some(Seen::Session(player, session))
            }
            _ => None,
        })
        .unwrap_or(Seen::Ended)
}

/// Whether the player keeps following the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    /// The session is over; the player leaves remote mode and syncs.
    Leave,
}

// ==============================================================================
// The session as shown
// ==============================================================================

/// The episode on a Sonos group, from the hand-off until it comes back or
/// ends.
#[derive(Debug)]
pub struct Remote {
    pub target: Target,
    /// Waiting for the group to answer the hand-off. Commands are ignored
    /// meanwhile, since there is no session to steer yet.
    connecting: bool,
    playing: bool,
    /// The last observed position and when it was seen, so the clock runs
    /// between polls.
    anchor_ms: i64,
    anchor_at: Instant,
    /// What the bar shows: the anchor plus the time since, while playing.
    shown_ms: i64,
    /// The group volume, once a player answer said it.
    volume: Option<u8>,
    volume_set_at: Option<Instant>,
    /// When the last command was sent, so a poll sent before it doesn't
    /// undo what it showed.
    commanded_at: Option<Instant>,
    /// When the group first went missing from the list.
    missing_since: Option<Instant>,
    /// When the last poll went out.
    polled_at: Option<Instant>,
    volume_queue: LatestOnly<u8>,
}

/// The session as shown before a command, for [`Remote::revert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    playing: bool,
    position_ms: i64,
}

impl Remote {
    /// A hand-off waiting for the group, showing `position_ms`, paused.
    pub fn connecting(target: Target, position_ms: i64, now: Instant) -> Remote {
        Remote {
            target,
            connecting: true,
            playing: false,
            anchor_ms: position_ms,
            anchor_at: now,
            shown_ms: position_ms,
            volume: None,
            volume_set_at: None,
            commanded_at: None,
            missing_since: None,
            polled_at: None,
            volume_queue: LatestOnly::default(),
        }
    }

    /// A session found already playing, as on start.
    pub fn following(player: &SonosPlayer, session: SonosSessionView, now: Instant) -> Remote {
        let mut remote = Remote::connecting(player.into(), session.position_ms, now);
        remote.connecting = false;
        remote.show(session.playing, session.position_ms, now);
        remote.volume = Some(player.volume);
        remote.polled_at = Some(now);
        remote
    }

    pub fn is_connecting(&self) -> bool {
        self.connecting
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn position_ms(&self) -> i64 {
        self.shown_ms
    }

    pub fn volume(&self) -> Option<u8> {
        self.volume
    }

    /// The group answered the hand-off with `player`. It was just told to
    /// play and may still be buffering, which the answer reports as not
    /// playing, so it shows as playing until the polls say otherwise.
    pub fn connected(&mut self, player: &SonosPlayer, episode: EpisodeId, now: Instant) {
        self.connecting = false;
        self.polled_at = Some(now);
        self.volume = Some(player.volume);
        let position_ms = match observe(std::slice::from_ref(player), episode, None) {
            Seen::Session(_, session) => session.position_ms,
            Seen::Ended | Seen::Missing => self.anchor_ms,
        };
        self.show(true, position_ms, now);
    }

    /// Runs the clock to `now`. Returns whether the shown second changed,
    /// which is when the bar needs a new frame.
    pub fn tick(&mut self, now: Instant, duration_ms: Option<i64>) -> bool {
        if !self.playing {
            return false;
        }
        let elapsed = i64::try_from(now.saturating_duration_since(self.anchor_at).as_millis())
            .unwrap_or(i64::MAX);
        let shown = self
            .anchor_ms
            .saturating_add(elapsed)
            .min(duration_ms.unwrap_or(i64::MAX));
        let changed = shown / 1000 != self.shown_ms / 1000;
        self.shown_ms = shown;
        changed
    }

    /// Whether a poll is due: [`POLL_IN_VIEW`] after the last one while
    /// the window is active, [`POLL_AWAY`] otherwise. A window coming back
    /// into view after a while polls at once. Marks the poll as sent.
    pub fn poll_due(&mut self, now: Instant, in_view: bool) -> bool {
        if self.connecting {
            return false;
        }
        let every = if in_view { POLL_IN_VIEW } else { POLL_AWAY };
        let due = self
            .polled_at
            .is_none_or(|at| now.saturating_duration_since(at) >= every);
        if due {
            self.polled_at = Some(now);
        }
        due
    }

    /// Shows what `command` will do before the group answers, and returns
    /// what showed before, for [`Remote::revert`]. Skips by `delta_ms`
    /// from the clock's guess at the group's position, within
    /// `duration_ms`.
    pub fn command(
        &mut self,
        command: SonosCommand,
        duration_ms: Option<i64>,
        now: Instant,
    ) -> Shown {
        let before = Shown {
            playing: self.playing,
            position_ms: self.shown_ms,
        };
        self.commanded_at = Some(now);
        let end = duration_ms.unwrap_or(i64::MAX);
        match command {
            SonosCommand::Pause => self.show(false, self.shown_ms, now),
            SonosCommand::Resume => self.show(true, self.shown_ms, now),
            SonosCommand::Seek { position_ms } => {
                self.show(self.playing, position_ms.clamp(0, end), now);
            }
            SonosCommand::Skip { delta_ms } => self.show(
                self.playing,
                self.shown_ms.saturating_add(delta_ms).clamp(0, end),
                now,
            ),
            SonosCommand::Volume { .. } => {}
        }
        before
    }

    /// The group couldn't be reached, so it never heard the command: shows
    /// the session as it was.
    pub fn revert(&mut self, before: Shown, now: Instant) {
        self.show(before.playing, before.position_ms, now);
    }

    /// A command's answer: the group as it is right after the command.
    pub fn answered(&mut self, player: &SonosPlayer, episode: EpisodeId, now: Instant) -> Verdict {
        self.follow(std::slice::from_ref(player), episode, now)
    }

    /// A poll's answer. One sent before the last command can't know about
    /// it and changes nothing.
    pub fn polled(
        &mut self,
        players: &[SonosPlayer],
        episode: EpisodeId,
        sent_at: Instant,
        now: Instant,
    ) -> Verdict {
        if self.commanded_at.is_some_and(|at| at > sent_at) {
            return Verdict::Keep;
        }
        self.follow(players, episode, now)
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
            // The next poll shows the volume the group really has.
            self.volume_set_at = None;
        }
        self.volume_queue.finished()
    }

    fn follow(&mut self, players: &[SonosPlayer], episode: EpisodeId, now: Instant) -> Verdict {
        if let Some(player) = players.iter().find(|player| player.id == self.target.id)
            && self
                .volume_set_at
                .is_none_or(|at| now.saturating_duration_since(at) >= VOLUME_HOLD)
        {
            self.volume = Some(player.volume);
        }
        match observe(players, episode, Some(&self.target.id)) {
            Seen::Session(_, session) => {
                self.missing_since = None;
                self.show(session.playing, session.position_ms, now);
                Verdict::Keep
            }
            Seen::Ended => Verdict::Leave,
            Seen::Missing => {
                let since = *self.missing_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= MISSING_LIMIT {
                    Verdict::Leave
                } else {
                    Verdict::Keep
                }
            }
        }
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
    use super::*;

    const EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c01";
    const OTHER: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c02";

    fn episode() -> EpisodeId {
        EPISODE.parse().expect("the id is valid")
    }

    fn couch(state: SonosPlayerState, session: Option<(&str, bool, i64)>) -> SonosPlayer {
        SonosPlayer {
            id: "RINCON_COUCH".to_owned(),
            name: "Couch".to_owned(),
            volume: 12,
            state,
            session: session.map(|(episode, playing, position_ms)| SonosSessionView {
                episode_id: episode.parse().expect("the id is valid"),
                playing,
                position_ms,
            }),
        }
    }

    fn playing_at(position_ms: i64) -> SonosPlayer {
        couch(SonosPlayerState::Pod, Some((EPISODE, true, position_ms)))
    }

    fn followed(now: Instant) -> Remote {
        let player = playing_at(60_000);
        let session = player.session.expect("the player has a session");
        Remote::following(&player, session, now)
    }

    #[test]
    fn a_hand_off_starts_where_a_local_play_would() {
        let unplayed = Stored {
            position_ms: 754_000,
            played: false,
        };
        assert_eq!(
            hand_off_start(unplayed),
            Start {
                position_ms: 754_000,
                unplay: false
            }
        );
        let played = Stored {
            position_ms: 754_000,
            played: true,
        };
        assert_eq!(
            hand_off_start(played),
            Start {
                position_ms: 0,
                unplay: true
            },
            "a played episode starts over and is marked unplayed first"
        );
    }

    #[test]
    fn the_session_is_read_from_its_group_or_any_group() {
        let players = [
            SonosPlayer {
                id: "RINCON_KITCHEN".to_owned(),
                ..couch(SonosPlayerState::Other, None)
            },
            playing_at(5_000),
        ];
        assert!(matches!(
            observe(&players, episode(), Some("RINCON_COUCH")),
            Seen::Session(player, session) if player.name == "Couch" && session.position_ms == 5_000
        ));
        assert!(matches!(
            observe(&players, episode(), None),
            Seen::Session(..)
        ));
        assert_eq!(
            observe(&players, episode(), Some("RINCON_KITCHEN")),
            Seen::Ended
        );
        assert_eq!(
            observe(&players, episode(), Some("RINCON_GONE")),
            Seen::Missing
        );
        let other_episode = [couch(SonosPlayerState::Pod, Some((OTHER, true, 5_000)))];
        assert_eq!(
            observe(&other_episode, episode(), Some("RINCON_COUCH")),
            Seen::Ended,
            "another episode's session has nothing to show"
        );
        assert_eq!(observe(&[], episode(), None), Seen::Ended);
    }

    #[test]
    fn the_clock_runs_between_polls_while_playing() {
        let start = Instant::now();
        let mut remote = followed(start);
        assert!(!remote.tick(start + Duration::from_millis(500), None));
        assert!(remote.tick(start + Duration::from_millis(1_000), None));
        assert_eq!(remote.position_ms(), 61_000);
        assert!(remote.tick(start + Duration::from_secs(10), Some(65_000)));
        assert_eq!(remote.position_ms(), 65_000, "never past the end");

        remote.command(SonosCommand::Pause, None, start + Duration::from_secs(10));
        assert!(!remote.tick(start + Duration::from_secs(20), None));
        assert_eq!(
            remote.position_ms(),
            65_000,
            "a paused session stands still"
        );
    }

    #[test]
    fn polls_follow_the_session_until_it_ends() {
        let start = Instant::now();
        let mut remote = followed(start);
        let later = start + Duration::from_secs(2);
        let paused = couch(SonosPlayerState::Pod, Some((EPISODE, false, 90_000)));
        assert_eq!(
            remote.polled(&[paused], episode(), later, later),
            Verdict::Keep
        );
        assert!(!remote.is_playing());
        assert_eq!(remote.position_ms(), 90_000);

        for over in [
            couch(SonosPlayerState::Idle, None),
            couch(SonosPlayerState::Other, None),
            couch(SonosPlayerState::Pod, Some((OTHER, true, 0))),
        ] {
            let mut remote = followed(start);
            assert_eq!(
                remote.polled(std::slice::from_ref(&over), episode(), later, later),
                Verdict::Leave,
                "{over:?}"
            );
        }
    }

    #[test]
    fn a_missing_group_keeps_the_session_for_two_minutes() {
        let start = Instant::now();
        let mut remote = followed(start);
        let poll = |remote: &mut Remote, at: Duration| {
            remote.polled(&[], episode(), start + at, start + at)
        };
        assert_eq!(poll(&mut remote, Duration::from_secs(2)), Verdict::Keep);
        assert_eq!(poll(&mut remote, Duration::from_secs(100)), Verdict::Keep);
        assert_eq!(
            poll(&mut remote, Duration::from_secs(122)),
            Verdict::Leave,
            "two minutes after it first went missing"
        );

        let mut remote = followed(start);
        assert_eq!(poll(&mut remote, Duration::from_secs(2)), Verdict::Keep);
        let back = start + Duration::from_secs(60);
        remote.polled(&[playing_at(70_000)], episode(), back, back);
        assert_eq!(
            poll(&mut remote, Duration::from_secs(150)),
            Verdict::Keep,
            "coming back resets the wait"
        );
    }

    #[test]
    fn a_poll_sent_before_a_command_changes_nothing() {
        let start = Instant::now();
        let mut remote = followed(start);
        let sent = start + Duration::from_secs(1);
        let commanded = start + Duration::from_secs(2);
        remote.command(SonosCommand::Pause, None, commanded);
        let answered = start + Duration::from_secs(3);
        assert_eq!(
            remote.polled(&[playing_at(61_000)], episode(), sent, answered),
            Verdict::Keep
        );
        assert!(!remote.is_playing(), "the pause still shows");
        assert_eq!(
            remote.polled(
                &[couch(SonosPlayerState::Idle, None)],
                episode(),
                sent,
                answered
            ),
            Verdict::Keep,
            "not even an end"
        );
    }

    #[test]
    fn commands_show_at_once_and_revert_when_unheard() {
        let start = Instant::now();
        let mut remote = followed(start);
        let before = remote.command(SonosCommand::Skip { delta_ms: -90_000 }, None, start);
        assert_eq!(remote.position_ms(), 0, "clamped at the start");
        remote.command(SonosCommand::Skip { delta_ms: 30_000 }, Some(20_000), start);
        assert_eq!(remote.position_ms(), 20_000, "clamped at the end");
        remote.command(SonosCommand::Seek { position_ms: 5_000 }, None, start);
        assert_eq!(remote.position_ms(), 5_000);
        remote.revert(before, start);
        assert_eq!((remote.position_ms(), remote.is_playing()), (60_000, true));
    }

    #[test]
    fn a_connecting_session_shows_playing_once_the_group_answers() {
        let start = Instant::now();
        let mut remote = Remote::connecting(
            Target {
                id: "RINCON_COUCH".to_owned(),
                name: "Couch".to_owned(),
            },
            754_000,
            start,
        );
        assert!(remote.is_connecting());
        assert!(!remote.poll_due(start, true), "nothing to poll yet");
        let buffering = couch(SonosPlayerState::Pod, Some((EPISODE, false, 754_000)));
        remote.connected(&buffering, episode(), start);
        assert!(!remote.is_connecting());
        assert!(remote.is_playing(), "buffering still shows as playing");
        assert_eq!(remote.position_ms(), 754_000);
        assert_eq!(remote.volume(), Some(12));
    }

    #[test]
    fn polls_come_every_two_seconds_in_view_and_fifteen_away() {
        let start = Instant::now();
        let mut remote = followed(start);
        let at = |seconds| start + Duration::from_secs(seconds);
        assert!(!remote.poll_due(at(1), true));
        assert!(remote.poll_due(at(2), true));
        assert!(!remote.poll_due(at(10), false));
        assert!(remote.poll_due(at(17), false));
        assert!(
            remote.poll_due(at(19), true),
            "back in view, it polls on the in-view schedule"
        );
    }

    #[test]
    fn polled_volume_waits_after_a_local_change() {
        let start = Instant::now();
        let mut remote = followed(start);
        assert_eq!(remote.set_volume(40, start), Some(40));
        let soon = start + Duration::from_secs(1);
        remote.polled(&[playing_at(61_000)], episode(), soon, soon);
        assert_eq!(remote.volume(), Some(40), "the poll's 12 is ignored");
        let later = start + VOLUME_HOLD;
        remote.polled(&[playing_at(63_000)], episode(), later, later);
        assert_eq!(remote.volume(), Some(12));
    }

    #[test]
    fn only_the_newest_volume_waits_while_one_is_out() {
        let start = Instant::now();
        let mut remote = followed(start);
        assert_eq!(remote.set_volume(20, start), Some(20));
        assert_eq!(remote.set_volume(25, start), None);
        assert_eq!(remote.set_volume(31, start), None);
        assert_eq!(remote.volume(), Some(31));
        assert_eq!(remote.volume_sent(true), Some(31));
        assert_eq!(remote.volume_sent(true), None);
        assert_eq!(remote.set_volume(40, start), Some(40), "idle again");
    }
}
