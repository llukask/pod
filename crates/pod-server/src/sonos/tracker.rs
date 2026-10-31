//! The session tracker's rule table (spec 13.3), as a pure function from a
//! session and one poll of its group to the writes and the next session.
//!
//! The speaker's reports drive everything: its transport state, whether it
//! still holds the session's URI, and `RelTime`. Feed durations are
//! approximate (one observed episode ran 24 s past its feed's), so the end
//! of an episode is the `STOPPED` that follows `PLAYING`, never a position.

use pod_protocol::{DeviceId, EpisodeId, Timestamp, UserId};

/// A group that hasn't answered for this long has its session ended.
pub const UNREACHABLE_LIMIT_MS: i64 = 2 * 60 * 1000;

/// `CurrentTransportState` from `GetTransportInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    Playing,
    PausedPlayback,
    Stopped,
    Transitioning,
    NoMediaPresent,
    /// Anything else a speaker reports, which the tracker waits out.
    Unknown,
}

impl TransportState {
    pub fn parse(state: &str) -> Self {
        match state.trim() {
            "PLAYING" => Self::Playing,
            "PAUSED_PLAYBACK" => Self::PausedPlayback,
            "STOPPED" => Self::Stopped,
            "TRANSITIONING" => Self::Transitioning,
            "NO_MEDIA_PRESENT" => Self::NoMediaPresent,
            _ => Self::Unknown,
        }
    }
}

/// A `sonos_session` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The coordinator's UUID.
    pub player_id: String,
    pub user_id: UserId,
    pub episode_id: EpisodeId,
    /// The URI sent to the speaker. The group still plays the session while
    /// its `CurrentURI` equals this.
    pub audio_url: String,
    /// `*_dev` of every write this session makes, and its identity: a new
    /// session on the same player gets a new one.
    pub device_id: DeviceId,
    pub duration_ms: Option<i64>,
    /// The last position observed while playing or paused.
    pub position_ms: i64,
    /// The last observation was `PLAYING`.
    pub playing: bool,
    pub played_written: bool,
}

/// A session plus what the tracker keeps in memory between polls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracked {
    pub session: Session,
    /// When the group first failed to answer in the current run of
    /// failures.
    pub unreachable_since: Option<Timestamp>,
}

/// What one poll of the group found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// The coordinator answered all three queries.
    Seen(Seen),
    /// The topology no longer lists the player as a coordinator: the group
    /// was merged into another.
    NotCoordinator,
    /// The coordinator, or every topology seed, didn't answer.
    Unreachable,
}

/// A coordinator's answers to `GetTransportInfo`, `GetMediaInfo` and
/// `GetPositionInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub state: TransportState,
    /// `CurrentURI` equals the session's `audio_url`.
    pub ours: bool,
    /// `RelTime`, in whole seconds.
    pub position_ms: i64,
}

impl Seen {
    /// The session's position as of this poll. The speaker's report counts
    /// only while it plays or pauses the session's episode: once it stops,
    /// it reports 0, and for anything else it reports someone else's track.
    pub fn position_for(&self, session: &Session) -> i64 {
        let live = matches!(
            self.state,
            TransportState::Playing | TransportState::PausedPlayback
        );
        if self.ours && live {
            self.position_ms
        } else {
            session.position_ms
        }
    }

    /// Whether the session plays as of this poll. A transition (buffering,
    /// a seek) keeps whatever the session did before.
    pub fn playing_for(&self, session: &Session) -> bool {
        self.ours
            && match self.state {
                TransportState::Playing => true,
                TransportState::Transitioning => session.playing,
                _ => false,
            }
    }
}

/// An `episode_state` field the step writes, stamped with the observation
/// time and the session's device id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    Position(i64),
    /// `played = true`.
    Played,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Keep(Tracked),
    /// Delete the session row.
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub writes: Vec<Write>,
    pub next: Next,
}

impl Step {
    fn keep(writes: Vec<Write>, tracked: Tracked) -> Self {
        Self {
            writes,
            next: Next::Keep(tracked),
        }
    }

    fn end(writes: Vec<Write>) -> Self {
        Self {
            writes,
            next: Next::End,
        }
    }
}

/// One poll of a session (the table of spec 13.3). `threshold_ms` is the
/// user's `played_threshold_ms`.
pub fn step(
    tracked: &Tracked,
    observation: Observation,
    now: Timestamp,
    threshold_ms: i64,
) -> Step {
    let session = &tracked.session;
    let seen = match observation {
        Observation::NotCoordinator => return Step::end(Vec::new()),
        Observation::Unreachable => {
            let since = tracked.unreachable_since.unwrap_or(now);
            if now.as_epoch_ms() - since.as_epoch_ms() >= UNREACHABLE_LIMIT_MS {
                return Step::end(Vec::new());
            }
            let next = Tracked {
                session: session.clone(),
                unreachable_since: Some(since),
            };
            return Step::keep(Vec::new(), next);
        }
        Observation::Seen(seen) => seen,
    };

    // The group answered, so any run of failures is over.
    let mut next = Tracked {
        session: session.clone(),
        unreachable_since: None,
    };
    if seen.state == TransportState::Transitioning {
        return Step::keep(Vec::new(), next);
    }
    if !seen.ours {
        // The group changed source. The last position is already written.
        return Step::end(Vec::new());
    }
    match seen.state {
        TransportState::Playing => {
            let mut writes = vec![Write::Position(seen.position_ms)];
            if !session.played_written
                && near_end(session.duration_ms, seen.position_ms, threshold_ms)
            {
                writes.push(Write::Played);
                next.session.played_written = true;
            }
            next.session.position_ms = seen.position_ms;
            next.session.playing = true;
            Step::keep(writes, next)
        }
        TransportState::PausedPlayback => {
            // The session stays, since the Sonos app can resume it.
            let writes = if session.playing {
                vec![Write::Position(seen.position_ms)]
            } else {
                Vec::new()
            };
            next.session.position_ms = seen.position_ms;
            next.session.playing = false;
            Step::keep(writes, next)
        }
        TransportState::Stopped if session.playing => {
            // The episode ended. The speaker reports position 0 now, which
            // must not overwrite the last position.
            let writes = if session.played_written {
                Vec::new()
            } else {
                vec![Write::Played]
            };
            Step::end(writes)
        }
        TransportState::Stopped
        | TransportState::Transitioning
        | TransportState::NoMediaPresent
        | TransportState::Unknown => Step::keep(Vec::new(), next),
    }
}

/// The played rule of spec 8: the remaining time is below the threshold or
/// 2 % of the duration, whichever is smaller. Without a duration there is
/// no threshold, and only the end of the episode marks it played.
fn near_end(duration_ms: Option<i64>, position_ms: i64, threshold_ms: i64) -> bool {
    duration_ms
        .is_some_and(|duration_ms| duration_ms - position_ms < threshold_ms.min(duration_ms / 50))
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD_MS: i64 = 30_000;
    /// 50 minutes, so 2 % (60 s) is above the 30 s threshold.
    const DURATION_MS: i64 = 50 * 60 * 1000;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_epoch_ms(1_800_000_000_000 + ms).expect("near today")
    }

    fn session(position_ms: i64, playing: bool) -> Tracked {
        Tracked {
            session: Session {
                player_id: "RINCON_000E5800000201400".into(),
                user_id: UserId::new_v7(),
                episode_id: EpisodeId::new(
                    pod_protocol::PodcastId::from_feed_url(
                        &pod_protocol::normalize_feed_url("https://feeds.example.org/show.xml")
                            .expect("valid feed URL"),
                    ),
                    "episode",
                ),
                audio_url: "https://media.example.org/episode.mp3".into(),
                device_id: DeviceId::new_v7(),
                duration_ms: Some(DURATION_MS),
                position_ms,
                playing,
                played_written: false,
            },
            unreachable_since: None,
        }
    }

    fn seen(state: TransportState, ours: bool, position_ms: i64) -> Observation {
        Observation::Seen(Seen {
            state,
            ours,
            position_ms,
        })
    }

    /// What a case expects: the writes, and the next session's position,
    /// `playing` and `played_written`, or `None` for an ended session.
    type Expected = (Vec<Write>, Option<(i64, bool, bool)>);

    fn run(tracked: &Tracked, observation: Observation) -> Expected {
        let step = step(tracked, observation, at(0), THRESHOLD_MS);
        let next = match step.next {
            Next::Keep(next) => Some((
                next.session.position_ms,
                next.session.playing,
                next.session.played_written,
            )),
            Next::End => None,
        };
        (step.writes, next)
    }

    #[test]
    fn the_rule_table() {
        use TransportState::*;
        use Write::*;

        let near_end = DURATION_MS - 20_000;
        let mut played_written = session(near_end, true);
        played_written.session.played_written = true;
        let mut no_duration = session(0, true);
        no_duration.session.duration_ms = None;

        let cases: Vec<(&str, Tracked, Observation, Expected)> = vec![
            (
                "PLAYING writes the position",
                session(10_000, true),
                seen(Playing, true, 15_000),
                (vec![Position(15_000)], Some((15_000, true, false))),
            ),
            (
                "PLAYING after a pause writes the position and plays again",
                session(10_000, false),
                seen(Playing, true, 10_000),
                (vec![Position(10_000)], Some((10_000, true, false))),
            ),
            (
                "PLAYING within the threshold of the end marks played once",
                session(near_end - 5_000, true),
                seen(Playing, true, near_end),
                (
                    vec![Position(near_end), Played],
                    Some((near_end, true, true)),
                ),
            ),
            (
                "PLAYING after played was written only writes the position",
                played_written.clone(),
                seen(Playing, true, near_end + 5_000),
                (
                    vec![Position(near_end + 5_000)],
                    Some((near_end + 5_000, true, true)),
                ),
            ),
            (
                "PLAYING without a duration never marks played by position",
                no_duration,
                seen(Playing, true, 3_600_000),
                (vec![Position(3_600_000)], Some((3_600_000, true, false))),
            ),
            (
                "PAUSED_PLAYBACK writes the position once on the change from playing",
                session(10_000, true),
                seen(PausedPlayback, true, 12_000),
                (vec![Position(12_000)], Some((12_000, false, false))),
            ),
            (
                "PAUSED_PLAYBACK while paused writes nothing and keeps the session",
                session(12_000, false),
                seen(PausedPlayback, true, 12_000),
                (vec![], Some((12_000, false, false))),
            ),
            (
                "TRANSITIONING does nothing",
                session(10_000, true),
                seen(Transitioning, true, 0),
                (vec![], Some((10_000, true, false))),
            ),
            (
                "TRANSITIONING does nothing even while the URI changes",
                session(10_000, true),
                seen(Transitioning, false, 0),
                (vec![], Some((10_000, true, false))),
            ),
            (
                "STOPPED after PLAYING marks played and ends; position 0 is never written",
                session(DURATION_MS - 90_000, true),
                seen(Stopped, true, 0),
                (vec![Played], None),
            ),
            (
                "the end of an episode is recognised by STOPPED even 24 s past the feed duration",
                played_written.clone(),
                seen(Stopped, true, 0),
                (vec![], None),
            ),
            (
                "STOPPED not after PLAYING keeps the session and writes nothing",
                session(12_000, false),
                seen(Stopped, true, 0),
                (vec![], Some((12_000, false, false))),
            ),
            (
                "a URI not ours ends the session without writes",
                session(10_000, true),
                seen(Playing, false, 99_000),
                (vec![], None),
            ),
            (
                "a URI not ours while stopped ends the session too",
                session(10_000, false),
                seen(Stopped, false, 0),
                (vec![], None),
            ),
            (
                "NO_MEDIA_PRESENT with our URI keeps the session",
                session(10_000, false),
                seen(NoMediaPresent, true, 0),
                (vec![], Some((10_000, false, false))),
            ),
            (
                "an unknown state keeps the session",
                session(10_000, true),
                seen(Unknown, true, 0),
                (vec![], Some((10_000, true, false))),
            ),
            (
                "a player that is no longer a coordinator ends the session",
                session(10_000, true),
                Observation::NotCoordinator,
                (vec![], None),
            ),
        ];
        for (name, tracked, observation, expected) in cases {
            assert_eq!(run(&tracked, observation), expected, "{name}");
        }
    }

    #[test]
    fn the_end_of_an_episode_is_recognised_past_the_feed_duration() {
        // The feed says 50 minutes; the file runs 24 s longer. Positions
        // past the duration are written as they come, and STOPPED ends it.
        let mut tracked = session(DURATION_MS - 1_000, true);
        let mut writes = Vec::new();
        for (state, position_ms) in [
            (TransportState::Playing, DURATION_MS + 4_000),
            (TransportState::Playing, DURATION_MS + 24_000),
            (TransportState::Stopped, 0),
        ] {
            let step = step(
                &tracked,
                seen(state, true, position_ms),
                at(0),
                THRESHOLD_MS,
            );
            writes.extend(step.writes);
            match step.next {
                Next::Keep(next) => tracked = next,
                Next::End => break,
            }
        }
        assert_eq!(
            writes,
            [
                Write::Position(DURATION_MS + 4_000),
                Write::Played,
                Write::Position(DURATION_MS + 24_000),
            ]
        );
    }

    #[test]
    fn an_unreachable_group_ends_the_session_after_two_minutes() {
        let tracked = session(10_000, true);
        let first = step(&tracked, Observation::Unreachable, at(0), THRESHOLD_MS);
        assert!(first.writes.is_empty());
        let Next::Keep(waiting) = first.next else {
            panic!("the first failure keeps the session");
        };
        assert_eq!(waiting.unreachable_since, Some(at(0)));
        assert_eq!(waiting.session, tracked.session);

        let later = step(
            &waiting,
            Observation::Unreachable,
            at(UNREACHABLE_LIMIT_MS - 1),
            THRESHOLD_MS,
        );
        assert_eq!(
            later.next,
            Next::Keep(waiting.clone()),
            "the clock keeps running"
        );

        let answered = step(
            &waiting,
            seen(TransportState::Playing, true, 15_000),
            at(UNREACHABLE_LIMIT_MS - 1),
            THRESHOLD_MS,
        );
        let Next::Keep(answered) = answered.next else {
            panic!("an answer keeps the session");
        };
        assert_eq!(
            answered.unreachable_since, None,
            "an answer resets the clock"
        );

        let gone = step(
            &waiting,
            Observation::Unreachable,
            at(UNREACHABLE_LIMIT_MS),
            THRESHOLD_MS,
        );
        assert_eq!(gone, Step::end(Vec::new()));
    }

    #[test]
    fn a_short_episode_uses_two_percent_of_its_duration() {
        // 10 minutes: 2 % is 12 s, below the 30 s threshold.
        let mut tracked = session(0, true);
        tracked.session.duration_ms = Some(600_000);
        let step_at = |position_ms| {
            step(
                &tracked,
                seen(TransportState::Playing, true, position_ms),
                at(0),
                THRESHOLD_MS,
            )
            .writes
        };
        assert_eq!(step_at(580_000), [Write::Position(580_000)]);
        assert_eq!(step_at(589_000), [Write::Position(589_000), Write::Played]);
    }

    #[test]
    fn views_trust_the_speaker_only_while_it_plays_the_session() {
        let tracked = session(42_000, true);
        let view = |state, ours, position_ms| {
            let seen = Seen {
                state,
                ours,
                position_ms,
            };
            (
                seen.position_for(&tracked.session),
                seen.playing_for(&tracked.session),
            )
        };
        assert_eq!(view(TransportState::Playing, true, 45_000), (45_000, true));
        assert_eq!(
            view(TransportState::PausedPlayback, true, 45_000),
            (45_000, false)
        );
        assert_eq!(view(TransportState::Transitioning, true, 0), (42_000, true));
        assert_eq!(view(TransportState::Stopped, true, 0), (42_000, false));
        assert_eq!(view(TransportState::Playing, false, 9_000), (42_000, false));
    }
}
