//! The reconciler's decision step (spec 7.4) as a pure function: a snapshot
//! of the download tables plus the conditions outside them (clock, network,
//! free space, the playing episode, which transfers are alive) in, a list of
//! [`Action`]s out. `conformance/downloads.json` drives it, and its runner
//! checks that a second pass over the result plans nothing.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::download::WaitReason;
use crate::net::NetworkState;

const MIB: u64 = 1024 * 1024;

/// What budget fitting assumes for an episode with no known size.
pub const UNKNOWN_SIZE: u64 = 100 * MIB;
/// Below this much free space every download pauses (spec 7.2).
pub const MIN_FREE: u64 = 500 * MIB;
pub const MAX_RUNNING: usize = 2;
pub const DEFAULT_BUDGET: u64 = 2 * 1024 * MIB;
pub const DEFAULT_GRACE_HOURS: f64 = 24.0;

// ==============================================================================
// Inputs
// ==============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Pin,
    Auto,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Pin => "pin",
            Source::Auto => "auto",
        }
    }

    pub fn parse(text: &str) -> Option<Source> {
        match text {
            "pin" => Some(Source::Pin),
            "auto" => Some(Source::Auto),
            _ => None,
        }
    }
}

/// `download.state` (spec 7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Queued,
    Transferring,
    WaitingRetry,
    Complete,
    Failed,
}

impl State {
    pub fn parse(text: &str) -> Option<State> {
        match text {
            "queued" => Some(State::Queued),
            "transferring" => Some(State::Transferring),
            "waiting_retry" => Some(State::WaitingRetry),
            "complete" => Some(State::Complete),
            "failed" => Some(State::Failed),
            _ => None,
        }
    }
}

/// A `podcast_device` row.
#[derive(Clone, Debug)]
pub struct PodcastRule {
    pub podcast_id: String,
    pub auto_keep: u32,
    pub auto_on_metered: bool,
}

/// An episode with its listening state, as far as wishes and sizes need it.
#[derive(Clone, Debug)]
pub struct Episode {
    pub id: String,
    pub podcast_id: String,
    pub published_at: i64,
    pub audio_bytes: Option<u64>,
    pub audio_url: String,
    pub played: bool,
    pub played_at: Option<i64>,
    pub archived: bool,
}

/// A `download_wish` row.
#[derive(Clone, Debug)]
pub struct Wish {
    pub episode_id: String,
    pub source: Source,
    pub created_at: i64,
    pub metered_ok: bool,
}

/// The `download` columns the plan reads.
#[derive(Clone, Debug)]
pub struct Download {
    pub episode_id: String,
    pub state: State,
    pub audio_url: String,
    /// The durable checkpoint, and for a `complete` row the file's size.
    pub bytes_done: u64,
    pub next_attempt_at: Option<i64>,
    pub wait_reason: Option<WaitReason>,
    pub unwanted_since: Option<i64>,
    pub run_id: Option<String>,
}

/// The device settings of spec 7.1, with their defaults filled in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub budget: u64,
    pub on_metered: bool,
    pub grace_ms: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            budget: DEFAULT_BUDGET,
            on_metered: false,
            grace_ms: hours_to_ms(DEFAULT_GRACE_HOURS),
        }
    }
}

pub fn hours_to_ms(hours: f64) -> i64 {
    (hours * 3_600_000.0) as i64
}

/// The download tables. `episodes` holds the episodes of podcasts with
/// `auto_keep > 0` and every episode a wish or row names, keyed by id.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub podcasts: Vec<PodcastRule>,
    pub episodes: HashMap<String, Episode>,
    pub wishes: Vec<Wish>,
    pub downloads: Vec<Download>,
    pub known_sizes: HashMap<String, u64>,
    pub settings: Settings,
}

/// Everything outside the tables a pass depends on.
#[derive(Clone, Debug)]
pub struct Conditions {
    pub now: i64,
    pub network: NetworkState,
    pub free_bytes: u64,
    pub playing: Option<String>,
    /// Episodes whose transfer job is still running. A `transferring` row
    /// missing here was orphaned by a crash or a cancelled job.
    pub live: HashSet<String>,
}

// ==============================================================================
// Output
// ==============================================================================

/// A row as the snapshot saw it. Row actions only apply while the row still
/// has this `run_id`, so a transfer that moved on in between (or a newer
/// run) isn't overwritten; the pass its end triggers looks again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowRef {
    pub episode_id: String,
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    AddWish {
        episode_id: String,
        source: Source,
        created_at: i64,
    },
    RemoveWish {
        episode_id: String,
        source: Source,
    },
    /// A new `queued` row.
    Insert {
        episode_id: String,
        audio_url: String,
    },
    SetUnwanted {
        row: RowRef,
        since: Option<i64>,
    },
    /// Only on `queued` rows.
    SetWait {
        row: RowRef,
        reason: Option<WaitReason>,
    },
    /// A due `waiting_retry` row, or an orphaned `transferring` one, back to
    /// `queued`.
    Requeue {
        row: RowRef,
    },
    /// The episode's `audio_url` changed: back to `queued` under no run,
    /// with the old run's files deleted.
    Reset {
        row: RowRef,
        audio_url: String,
    },
    /// Stops a live transfer and leaves its row `queued` with `reason`.
    Pause {
        row: RowRef,
        reason: WaitReason,
    },
    /// Deletes the row and its run's files, cancelling a live transfer.
    Remove {
        row: RowRef,
    },
    /// Moves a `queued` row to `transferring` under a new run and starts it.
    Start {
        row: RowRef,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// Auto wishes left out by budget fitting, for "skipped: storage budget".
    pub skipped: BTreeSet<String>,
    /// When a pass should run even if nothing else triggers one (epoch ms).
    pub wake_at: Option<i64>,
}

// ==============================================================================
// Planning
// ==============================================================================

/// How often a pass checks again while downloads wait for free space,
/// which can return without any event the reconciler sees.
const SPACE_RECHECK_MS: i64 = 60_000;

pub fn plan(snapshot: &Snapshot, conditions: &Conditions) -> Plan {
    let Snapshot {
        podcasts,
        episodes,
        wishes,
        downloads,
        known_sizes,
        settings,
    } = snapshot;
    let now = conditions.now;
    let mut actions = Vec::new();

    // --------------------------------------------------------------------
    // Wishes
    // --------------------------------------------------------------------

    // The auto wishes are exactly the newest `auto_keep` unplayed,
    // unarchived episodes of each podcast. Turning `auto_keep` down or off
    // therefore drops the rest, whichever podcast they belong to.
    let mut autos: HashSet<&str> = HashSet::new();
    for rule in podcasts.iter().filter(|rule| rule.auto_keep > 0) {
        let mut candidates: Vec<&Episode> = episodes
            .values()
            .filter(|episode| {
                episode.podcast_id == rule.podcast_id && !episode.played && !episode.archived
            })
            .collect();
        candidates.sort_by(|a, b| newest_first(a, b));
        autos.extend(
            candidates
                .iter()
                .take(rule.auto_keep as usize)
                .map(|episode| episode.id.as_str()),
        );
    }
    let mut kept: Vec<&Wish> = Vec::new();
    for wish in wishes {
        let episode = episodes.get(&wish.episode_id);
        let keep = match (wish.source, episode) {
            // Unsubscribing removes the episodes, and with them the wishes.
            (_, None) => false,
            (Source::Auto, Some(_)) => autos.contains(wish.episode_id.as_str()),
            // Finishing an episode releases its pin; pinning an already
            // played episode sticks until it is played through again.
            (Source::Pin, Some(episode)) => {
                !(episode.played && episode.played_at.is_some_and(|at| at > wish.created_at))
            }
        };
        if keep {
            kept.push(wish);
        } else {
            actions.push(Action::RemoveWish {
                episode_id: wish.episode_id.clone(),
                source: wish.source,
            });
        }
    }
    let mut new_autos: Vec<&str> = autos
        .iter()
        .copied()
        .filter(|id| {
            !wishes
                .iter()
                .any(|wish| wish.source == Source::Auto && wish.episode_id == *id)
        })
        .collect();
    new_autos.sort_unstable();
    let new_wishes: Vec<Wish> = new_autos
        .into_iter()
        .map(|episode_id| Wish {
            episode_id: episode_id.to_owned(),
            source: Source::Auto,
            created_at: now,
            metered_ok: false,
        })
        .collect();
    for wish in &new_wishes {
        actions.push(Action::AddWish {
            episode_id: wish.episode_id.clone(),
            source: Source::Auto,
            created_at: now,
        });
    }
    kept.extend(&new_wishes);

    // --------------------------------------------------------------------
    // Budget fitting, before anything downloads, so a pass never fetches a
    // file the next one evicts.
    // --------------------------------------------------------------------

    let size = |episode_id: &str| -> u64 {
        known_sizes
            .get(episode_id)
            .copied()
            .or_else(|| {
                episodes
                    .get(episode_id)
                    .and_then(|episode| episode.audio_bytes)
            })
            .unwrap_or(UNKNOWN_SIZE)
    };
    let row_of = |episode_id: &str| downloads.iter().find(|row| row.episode_id == episode_id);

    let mut pins: Vec<&Wish> = kept
        .iter()
        .copied()
        .filter(|wish| wish.source == Source::Pin)
        .collect();
    pins.sort_by(|a, b| (a.created_at, &a.episode_id).cmp(&(b.created_at, &b.episode_id)));
    let pinned: HashSet<&str> = pins.iter().map(|wish| wish.episode_id.as_str()).collect();
    let mut auto_episodes: Vec<&Episode> = kept
        .iter()
        .filter(|wish| wish.source == Source::Auto && !pinned.contains(wish.episode_id.as_str()))
        .filter_map(|wish| episodes.get(&wish.episode_id))
        .collect();
    auto_episodes.sort_by(|a, b| newest_first(a, b));

    let wished: HashSet<&str> = kept.iter().map(|wish| wish.episode_id.as_str()).collect();
    let playing = conditions.playing.as_deref();
    let pinned_extra = playing
        .filter(|id| !wished.contains(id))
        .and_then(row_of)
        .filter(|row| row.state == State::Complete)
        .map_or(0, |row| row.bytes_done);

    let mut target: Vec<&str> = pins.iter().map(|wish| wish.episode_id.as_str()).collect();
    let mut target_bytes: u64 = target.iter().map(|id| size(id)).sum();
    let mut skipped = BTreeSet::new();
    for episode in auto_episodes {
        let bytes = size(&episode.id);
        if target_bytes + bytes + pinned_extra <= settings.budget {
            target.push(&episode.id);
            target_bytes += bytes;
        } else {
            skipped.insert(episode.id.clone());
        }
    }
    let in_target: HashSet<&str> = target.iter().copied().collect();
    let protected = |id: &str| in_target.contains(id) || playing == Some(id);

    // --------------------------------------------------------------------
    // Rows: bookkeeping, removal, grace eviction, retries and orphans
    // --------------------------------------------------------------------

    /// A row as it stands after this pass's earlier actions.
    #[derive(Debug)]
    struct Row {
        row: RowRef,
        state: State,
        bytes_done: u64,
        wait_reason: Option<WaitReason>,
        unwanted_since: Option<i64>,
    }
    let mut kept_rows: Vec<Row> = Vec::new();
    let mut wake_at: Option<i64> = None;
    let mut wake = |at: i64| wake_at = Some(wake_at.map_or(at, |earlier: i64| earlier.min(at)));
    let live = |id: &str| conditions.live.contains(id);

    for download in downloads {
        let id = download.episode_id.as_str();
        let row = RowRef {
            episode_id: download.episode_id.clone(),
            run_id: download.run_id.clone(),
        };
        // A file counts as wanted while protected; a download that hasn't
        // finished only while targeted, so the playing episode is never
        // downloaded for being played.
        let wanted = match download.state {
            State::Complete => protected(id),
            _ => in_target.contains(id),
        };
        if !wanted && download.state != State::Complete {
            actions.push(Action::Remove { row });
            continue;
        }
        let unwanted_since = if protected(id) {
            None
        } else {
            Some(download.unwanted_since.unwrap_or(now))
        };
        if unwanted_since != download.unwanted_since {
            actions.push(Action::SetUnwanted {
                row: row.clone(),
                since: unwanted_since,
            });
        }
        if let Some(since) = unwanted_since {
            let expires = since.saturating_add(settings.grace_ms);
            if now >= expires {
                actions.push(Action::Remove { row });
                continue;
            }
            wake(expires);
        }

        let mut current = Row {
            row,
            state: download.state,
            bytes_done: download.bytes_done,
            wait_reason: download.wait_reason,
            unwanted_since,
        };
        // Only a targeted row restarts; a playing file outside the target
        // stays as it is until it is released.
        if let Some(episode) = episodes.get(id).filter(|_| in_target.contains(id))
            && episode.audio_url != download.audio_url
        {
            actions.push(Action::Reset {
                row: current.row.clone(),
                audio_url: episode.audio_url.clone(),
            });
            current.row.run_id = None;
            current.state = State::Queued;
            current.bytes_done = 0;
            current.wait_reason = None;
        }
        let due = match current.state {
            State::Transferring => !live(id),
            State::WaitingRetry => match download.next_attempt_at {
                Some(at) if at > now => {
                    wake(at);
                    false
                }
                _ => true,
            },
            _ => false,
        };
        if due {
            actions.push(Action::Requeue {
                row: current.row.clone(),
            });
            current.state = State::Queued;
            current.wait_reason = None;
        }
        kept_rows.push(current);
    }
    for id in &target {
        if row_of(id).is_none() {
            let episode = episodes
                .get(*id)
                .expect("targets come from existing episodes");
            actions.push(Action::Insert {
                episode_id: episode.id.clone(),
                audio_url: episode.audio_url.clone(),
            });
            kept_rows.push(Row {
                row: RowRef {
                    episode_id: episode.id.clone(),
                    run_id: None,
                },
                state: State::Queued,
                bytes_done: 0,
                wait_reason: None,
                unwanted_since: None,
            });
        }
    }

    // --------------------------------------------------------------------
    // Transfers
    // --------------------------------------------------------------------

    let metered_ok = |id: &str| {
        let network = conditions.network;
        let pin = kept
            .iter()
            .find(|wish| wish.source == Source::Pin && wish.episode_id == id);
        let podcast_allows = || {
            episodes.get(id).is_some_and(|episode| {
                podcasts
                    .iter()
                    .any(|rule| rule.podcast_id == episode.podcast_id && rule.auto_on_metered)
            })
        };
        !network.metered
            || settings.on_metered
            || pin.is_some_and(|pin| pin.metered_ok)
            || (pin.is_none() && podcast_allows())
    };
    let blocked = if !conditions.network.connected {
        Some(WaitReason::Connection)
    } else if conditions.free_bytes < MIN_FREE {
        Some(WaitReason::Space)
    } else {
        None
    };
    if blocked == Some(WaitReason::Space) {
        wake(now + SPACE_RECHECK_MS);
    }

    // A running transfer that couldn't start now stops. Disk full is the
    // spec's case; a metered network and a lost connection pause too, so
    // nothing downloads over mobile data behind the user's back.
    let mut running = 0;
    for current in &mut kept_rows {
        let id = current.row.episode_id.as_str();
        if current.state != State::Transferring {
            continue;
        }
        let pause = blocked.or_else(|| (!metered_ok(id)).then_some(WaitReason::Wifi));
        match pause {
            Some(reason) => {
                actions.push(Action::Pause {
                    row: current.row.clone(),
                    reason,
                });
                current.state = State::Queued;
                current.wait_reason = Some(reason);
            }
            None => running += 1,
        }
    }

    // Count what is really on disk: finished files, files in their grace
    // period, the playing file and every partial file. A running transfer
    // counts at its full size, so two starts in one pass can't both claim
    // the same room.
    let remaining =
        |current: &Row| size(&current.row.episode_id).saturating_sub(current.bytes_done);
    let mut usage: u64 = kept_rows
        .iter()
        .map(|current| {
            if current.state == State::Transferring {
                current.bytes_done + remaining(current)
            } else {
                current.bytes_done
            }
        })
        .sum();
    let mut evictable: Vec<&Row> = kept_rows
        .iter()
        .filter(|current| current.state == State::Complete && current.unwanted_since.is_some())
        .collect();
    evictable.sort_by_key(|current| (current.unwanted_since, current.row.episode_id.clone()));
    let mut evictable = evictable.into_iter();

    let mut waits: Vec<(RowRef, Option<WaitReason>, Option<WaitReason>)> = Vec::new();
    let mut starts: Vec<RowRef> = Vec::new();
    let mut evictions: Vec<RowRef> = Vec::new();
    let mut budget_full = false;
    for id in &target {
        let Some(current) = kept_rows
            .iter()
            .find(|current| current.row.episode_id == *id && current.state == State::Queued)
        else {
            continue;
        };
        // A row whose job is still ending (paused, or its outcome not yet
        // seen) waits for it; a second job would share its `.part`.
        if live(id) {
            continue;
        }
        let reason = if blocked.is_some() {
            blocked
        } else if !metered_ok(id) {
            Some(WaitReason::Wifi)
        } else if running >= MAX_RUNNING {
            None
        } else if budget_full {
            Some(WaitReason::Budget)
        } else {
            let needed = remaining(current);
            while usage + needed > settings.budget {
                let Some(oldest) = evictable.next() else {
                    break;
                };
                usage -= oldest.bytes_done;
                evictions.push(oldest.row.clone());
            }
            if usage + needed > settings.budget && !pinned.contains(id) {
                budget_full = true;
                Some(WaitReason::Budget)
            } else {
                usage += needed;
                running += 1;
                starts.push(current.row.clone());
                continue;
            }
        };
        waits.push((current.row.clone(), current.wait_reason, reason));
    }
    actions.extend(evictions.into_iter().map(|row| Action::Remove { row }));
    actions.extend(
        waits
            .into_iter()
            .filter(|(_, before, after)| before != after)
            .map(|(row, _, reason)| Action::SetWait { row, reason }),
    );
    actions.extend(starts.into_iter().map(|row| Action::Start { row }));

    Plan {
        actions,
        skipped,
        wake_at,
    }
}

/// Newest published first, ties broken by the larger id (spec 6's order).
fn newest_first(a: &Episode, b: &Episode) -> std::cmp::Ordering {
    (b.published_at, &b.id).cmp(&(a.published_at, &a.id))
}
