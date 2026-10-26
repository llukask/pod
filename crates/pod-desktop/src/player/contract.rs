//! The playback contract of spec 8 as a pure state machine: what the player
//! reports in (progress, pause, seek, stop, the end of the file) and the
//! `episode_state` writes that follow. The clock comes in with every call,
//! so tests drive it with made-up instants and no player.
//!
//! One [`Contract`] covers one play of one episode, from the moment it
//! starts until it stops, changes or ends.

use std::time::{Duration, Instant};

/// How often a playing episode saves its position (spec 8).
pub const SAVE_EVERY: Duration = Duration::from_secs(5);

/// A write to the playing episode's state, made through the local writes
/// API (spec 5.2) by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// `while_playing` marks the periodic saves, which may wait for a sync.
    Position {
        position_ms: i64,
        while_playing: bool,
    },
    Played(bool),
}

/// The episode's state as stored when playback starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stored {
    pub position_ms: i64,
    pub played: bool,
}

impl Stored {
    /// Where playback starts: the stored position, or the beginning of a
    /// played episode.
    pub fn start_position_ms(self) -> i64 {
        if self.played { 0 } else { self.position_ms }
    }
}

#[derive(Debug)]
pub struct Contract {
    /// `played_threshold_ms` from the synced settings.
    threshold_ms: i64,
    duration_ms: Option<i64>,
    position_ms: i64,
    playing: bool,
    /// When the last position write was made, or playback last resumed.
    /// The next periodic save is due [`SAVE_EVERY`] after it.
    saved_at: Instant,
    /// Whether this play wrote `played = true`, which it does once.
    marked_played: bool,
}

impl Contract {
    /// Starts playing at [`Stored::start_position_ms`]. Replaying a played
    /// episode writes `played = false` as its own mutation, so coalescing
    /// position saves can never lose the reopen.
    pub fn start(
        stored: Stored,
        duration_ms: Option<i64>,
        threshold_ms: i64,
        now: Instant,
    ) -> (Contract, Vec<Write>) {
        let contract = Contract {
            threshold_ms,
            duration_ms,
            position_ms: stored.start_position_ms(),
            playing: true,
            saved_at: now,
            marked_played: false,
        };
        let writes = if stored.played {
            vec![Write::Played(false)]
        } else {
            Vec::new()
        };
        (contract, writes)
    }

    pub fn position_ms(&self) -> i64 {
        self.position_ms
    }

    pub fn duration_ms(&self) -> Option<i64> {
        self.duration_ms
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// The player's own duration, which beats the feed's.
    pub fn set_duration(&mut self, duration_ms: i64) {
        self.duration_ms = Some(duration_ms);
    }

    /// The player reports where it is. Saves the position every
    /// [`SAVE_EVERY`] while playing.
    pub fn progress(&mut self, position_ms: i64, now: Instant) -> Vec<Write> {
        self.position_ms = position_ms;
        let mut writes = self.check_played();
        if self.playing && now.duration_since(self.saved_at) >= SAVE_EVERY {
            writes.push(self.save(true, now));
        }
        writes
    }

    pub fn pause(&mut self, now: Instant) -> Vec<Write> {
        if !self.playing {
            return Vec::new();
        }
        self.playing = false;
        vec![self.save(false, now)]
    }

    pub fn resume(&mut self, now: Instant) {
        if !self.playing {
            self.playing = true;
            self.saved_at = now;
        }
    }

    pub fn seek(&mut self, position_ms: i64, now: Instant) -> Vec<Write> {
        self.position_ms = position_ms.max(0);
        let mut writes = self.check_played();
        writes.push(self.save(false, now));
        writes
    }

    /// Stop, episode change or quit: the last position write of this play.
    pub fn stop(mut self, now: Instant) -> Vec<Write> {
        vec![self.save(false, now)]
    }

    /// The player reached the end of the file. Marks the episode played if
    /// the threshold didn't, which happens without a known duration.
    pub fn ended(mut self, now: Instant) -> Vec<Write> {
        if let Some(duration_ms) = self.duration_ms {
            self.position_ms = self.position_ms.max(duration_ms);
        }
        let mut writes = Vec::new();
        if !self.marked_played {
            writes.push(Write::Played(true));
        }
        writes.push(self.save(false, now));
        writes
    }

    fn save(&mut self, while_playing: bool, now: Instant) -> Write {
        self.saved_at = now;
        Write::Position {
            position_ms: self.position_ms,
            while_playing,
        }
    }

    /// `played = true`, once, when the remaining time drops below
    /// `played_threshold_ms` or 2 % of the duration, whichever is smaller.
    fn check_played(&mut self) -> Vec<Write> {
        let Some(duration_ms) = self.duration_ms else {
            return Vec::new();
        };
        let limit = self.threshold_ms.min(duration_ms / 50);
        if self.marked_played || duration_ms - self.position_ms >= limit {
            return Vec::new();
        }
        self.marked_played = true;
        vec![Write::Played(true)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD_MS: i64 = 30_000;
    /// 50 minutes, so 2 % (60 s) is above the 30 s threshold.
    const LONG_MS: i64 = 50 * 60 * 1000;

    fn secs(base: Instant, seconds: f64) -> Instant {
        base + Duration::from_secs_f64(seconds)
    }

    fn unplayed_at(position_ms: i64) -> Stored {
        Stored {
            position_ms,
            played: false,
        }
    }

    fn position(position_ms: i64, while_playing: bool) -> Write {
        Write::Position {
            position_ms,
            while_playing,
        }
    }

    #[test]
    fn playback_starts_at_the_stored_position_without_writing() {
        let now = Instant::now();
        let (contract, writes) =
            Contract::start(unplayed_at(754_000), Some(LONG_MS), THRESHOLD_MS, now);
        assert_eq!(contract.position_ms(), 754_000);
        assert!(contract.is_playing());
        assert_eq!(writes, []);
    }

    #[test]
    fn replaying_a_played_episode_starts_over_and_unmarks_it_once() {
        let now = Instant::now();
        let stored = Stored {
            position_ms: LONG_MS,
            played: true,
        };
        assert_eq!(stored.start_position_ms(), 0);
        let (mut contract, writes) = Contract::start(stored, Some(LONG_MS), THRESHOLD_MS, now);
        assert_eq!(contract.position_ms(), 0);
        assert_eq!(writes, [Write::Played(false)]);
        assert_eq!(
            contract.progress(1_000, secs(now, 1.0)),
            [],
            "only the start writes played = false"
        );
    }

    #[test]
    fn a_playing_episode_saves_its_position_every_five_seconds() {
        let now = Instant::now();
        let (mut contract, _) = Contract::start(unplayed_at(0), Some(LONG_MS), THRESHOLD_MS, now);
        let mut saves = Vec::new();
        for tick in 1..=24 {
            let at = secs(now, f64::from(tick) * 0.5);
            saves.extend(contract.progress(i64::from(tick) * 500, at));
        }
        assert_eq!(
            saves,
            [position(5_000, true), position(10_000, true)],
            "12 seconds of progress save at 5 s and 10 s"
        );
    }

    #[test]
    fn progress_while_paused_saves_nothing() {
        let now = Instant::now();
        let (mut contract, _) = Contract::start(unplayed_at(0), Some(LONG_MS), THRESHOLD_MS, now);
        assert_eq!(contract.pause(secs(now, 1.0)), [position(0, false)]);
        assert_eq!(contract.pause(secs(now, 2.0)), [], "already paused");
        assert_eq!(contract.progress(0, secs(now, 30.0)), []);
        assert!(!contract.is_playing());
    }

    #[test]
    fn resuming_restarts_the_five_second_timer() {
        let now = Instant::now();
        let (mut contract, _) = Contract::start(unplayed_at(0), Some(LONG_MS), THRESHOLD_MS, now);
        contract.progress(4_000, secs(now, 4.0));
        contract.pause(secs(now, 4.0));
        contract.resume(secs(now, 60.0));
        assert_eq!(contract.progress(8_000, secs(now, 64.0)), []);
        assert_eq!(
            contract.progress(9_000, secs(now, 65.0)),
            [position(9_000, true)]
        );
    }

    #[test]
    fn pause_seek_and_stop_save_at_once_and_reset_the_timer() {
        let now = Instant::now();
        let (mut contract, _) =
            Contract::start(unplayed_at(60_000), Some(LONG_MS), THRESHOLD_MS, now);
        contract.progress(63_000, secs(now, 3.0));
        assert_eq!(
            contract.seek(93_000, secs(now, 3.0)),
            [position(93_000, false)]
        );
        assert_eq!(
            contract.progress(97_000, secs(now, 7.0)),
            [],
            "the seek saved, so the next periodic save is 5 s after it"
        );
        assert_eq!(
            contract.seek(-5_000, secs(now, 7.5)),
            [position(0, false)],
            "a skip back past the start lands on 0"
        );
        assert_eq!(contract.stop(secs(now, 8.0)), [position(0, false)]);
    }

    #[test]
    fn crossing_the_threshold_marks_played_once() {
        let now = Instant::now();
        let (mut contract, _) = Contract::start(
            unplayed_at(LONG_MS - 31_000),
            Some(LONG_MS),
            THRESHOLD_MS,
            now,
        );
        assert_eq!(contract.progress(LONG_MS - 30_000, secs(now, 1.0)), []);
        assert_eq!(
            contract.progress(LONG_MS - 29_999, secs(now, 2.0)),
            [Write::Played(true)]
        );
        assert_eq!(contract.progress(LONG_MS - 29_000, secs(now, 3.0)), []);
        assert_eq!(
            contract.ended(secs(now, 4.0)),
            [position(LONG_MS, false)],
            "the end doesn't mark it again"
        );
    }

    #[test]
    fn a_short_episode_uses_two_percent_of_its_duration() {
        let now = Instant::now();
        // 10 minutes: 2 % is 12 s, below the 30 s threshold.
        let duration = 600_000;
        let (mut contract, _) = Contract::start(unplayed_at(0), Some(duration), THRESHOLD_MS, now);
        assert_eq!(
            contract.seek(duration - 20_000, now),
            [position(580_000, false)]
        );
        assert_eq!(
            contract.seek(duration - 11_000, now),
            [Write::Played(true), position(589_000, false)],
            "a seek past the threshold marks it too"
        );
    }

    #[test]
    fn the_players_duration_replaces_the_feeds() {
        let now = Instant::now();
        let (mut contract, _) = Contract::start(unplayed_at(0), None, THRESHOLD_MS, now);
        assert_eq!(
            contract.progress(LONG_MS, secs(now, 1.0)),
            [],
            "without a duration there's no threshold"
        );
        contract.set_duration(LONG_MS + 10_000);
        assert_eq!(contract.duration_ms(), Some(LONG_MS + 10_000));
        assert_eq!(
            contract.progress(LONG_MS, secs(now, 2.0)),
            [Write::Played(true)]
        );
    }

    #[test]
    fn reaching_the_end_marks_played_and_saves_the_end() {
        let now = Instant::now();
        let (contract, _) = Contract::start(unplayed_at(0), None, THRESHOLD_MS, now);
        assert_eq!(
            contract.ended(secs(now, 1.0)),
            [Write::Played(true), position(0, false)]
        );

        let (mut contract, _) = Contract::start(unplayed_at(0), Some(LONG_MS), THRESHOLD_MS, now);
        contract.progress(LONG_MS - 40_000, secs(now, 1.0));
        assert_eq!(
            contract.ended(secs(now, 2.0)),
            [Write::Played(true), position(LONG_MS, false)]
        );
    }
}
