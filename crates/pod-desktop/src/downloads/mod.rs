//! The downloads page's model (wireframe D4, spec 7.4): every episode a
//! download row or a wish names, the state each one is in, and the actions
//! that get out of it.
//!
//! The UI only writes `download_wish`, `download` and the episode's archived
//! flag; the reconciler does the rest. [`status`] and [`actions`] are pure,
//! so each state's label and way out are unit-tested here.

pub mod view;

use std::collections::HashMap;

use color_eyre::eyre::{Result, WrapErr as _};
use pod_protocol::{EpisodeId, PodcastId, Timestamp};
use rusqlite::{Connection, params};

use crate::download::{MAX_ATTEMPTS, Progress, WaitReason};
use crate::reconcile::plan::{Fit, Source, State};
use crate::store::{Store, Table, TableSet};
use crate::sync::SyncHandle;
use crate::sync::local_write::{self, EpisodeChange, LocalChange};
use crate::sync::session::format_bytes;

// ==============================================================================
// The model
// ==============================================================================

/// One line of the downloads table, as loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub episode_id: EpisodeId,
    pub podcast_id: PodcastId,
    pub title: String,
    pub podcast_title: String,
    pub pinned: bool,
    pub auto: bool,
    /// `None` for a wish the reconciler hasn't queued: a new one, or an
    /// auto wish skipped for the budget.
    pub row: Option<Row>,
    /// `(bytes, exact)`: exact from the row or `known_size`, else the
    /// feed's `audio_bytes`.
    pub size: Option<(u64, bool)>,
}

/// The `download` columns the page shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub state: State,
    pub wait_reason: Option<WaitReason>,
    pub attempts: u32,
    pub next_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    /// When the grace period of an unwanted finished file ends.
    pub leaving_at: Option<i64>,
}

/// What the page says an episode's download is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// An auto wish budget fitting left out.
    Skipped,
    Queued(Option<WaitReason>),
    Downloading {
        done: u64,
        total: Option<u64>,
    },
    /// `attempts` transient failures so far; the next try is due `at`.
    Retrying {
        at: Option<i64>,
        attempts: u32,
        error: Option<String>,
    },
    Failed {
        error: Option<String>,
    },
    /// `leaving_at` is set while the file waits out its grace period.
    Downloaded {
        leaving_at: Option<i64>,
    },
}

/// A way out of a state, as a button on the row and in the detail pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Pins an episode nothing wishes yet (the detail pane's button).
    Download,
    /// Stops a download that hasn't finished.
    Cancel,
    /// Deletes a failed row or a finished file.
    Remove,
    /// A failed row back to the queue.
    Retry,
    /// Pins the episode with `metered_ok` (spec 7.1).
    UseMetered,
    /// Pins a skipped or budget-held auto download.
    DownloadAnyway,
    /// Pins a finished file in its grace period.
    Keep,
    /// Archives a skipped auto download, so the rule stops picking it.
    Archive,
}

impl Act {
    pub fn label(self) -> &'static str {
        match self {
            Act::Download => "Download",
            Act::Cancel => "Cancel",
            Act::Remove => "Remove",
            Act::Retry => "Retry",
            Act::UseMetered => "Use metered network",
            Act::DownloadAnyway => "Download anyway",
            Act::Keep => "Keep",
            Act::Archive => "Archive",
        }
    }

    /// The suffix of the button's element id.
    pub fn key(self) -> &'static str {
        match self {
            Act::Download => "download",
            Act::Cancel => "cancel",
            Act::Remove => "remove",
            Act::Retry => "retry",
            Act::UseMetered => "metered",
            Act::DownloadAnyway => "anyway",
            Act::Keep => "keep",
            Act::Archive => "archive",
        }
    }
}

/// The status of `entry`, with live byte progress where a transfer runs.
pub fn status(entry: &Entry, fit: &Fit, progress: &HashMap<String, Progress>) -> Status {
    let Some(row) = &entry.row else {
        return if fit.skipped.contains(&entry.episode_id.to_string()) {
            Status::Skipped
        } else {
            Status::Queued(None)
        };
    };
    match row.state {
        State::Queued => Status::Queued(row.wait_reason),
        State::Transferring => {
            let live = progress.get(&entry.episode_id.to_string());
            Status::Downloading {
                done: live.map_or(row.bytes_done, |live| live.bytes_done),
                total: live.and_then(|live| live.bytes_total).or(row.bytes_total),
            }
        }
        State::WaitingRetry => Status::Retrying {
            at: row.next_attempt_at,
            attempts: row.attempts,
            error: row.last_error.clone(),
        },
        State::Failed => Status::Failed {
            error: row.last_error.clone(),
        },
        State::Complete => Status::Downloaded {
            leaving_at: row.leaving_at,
        },
    }
}

/// Every state's way out (D4: "Every row has a way out").
pub fn actions(status: &Status) -> &'static [Act] {
    match status {
        Status::Skipped => &[Act::DownloadAnyway, Act::Archive],
        Status::Queued(Some(WaitReason::Wifi)) => &[Act::UseMetered, Act::Cancel],
        // Pins never wait for the budget, so this is an auto download.
        Status::Queued(Some(WaitReason::Budget)) => &[Act::DownloadAnyway, Act::Cancel],
        Status::Queued(_) | Status::Downloading { .. } | Status::Retrying { .. } => &[Act::Cancel],
        Status::Failed { .. } => &[Act::Retry, Act::Remove],
        Status::Downloaded {
            leaving_at: Some(_),
        } => &[Act::Keep, Act::Remove],
        Status::Downloaded { leaving_at: None } => &[Act::Remove],
    }
}

/// The status column's text at `now` (epoch ms).
pub fn status_label(status: &Status, now: i64) -> String {
    match status {
        Status::Skipped => "Skipped · doesn't fit the budget".to_owned(),
        Status::Queued(None) => "Queued".to_owned(),
        Status::Queued(Some(reason)) => match reason {
            WaitReason::Connection => "Waiting · no connection",
            WaitReason::Wifi => "Waiting · metered network",
            WaitReason::Space => "Paused · storage full",
            WaitReason::Budget => "Waiting · doesn't fit the budget",
        }
        .to_owned(),
        // TODO: the transfer rate, as in D4 ("2.1 MB/s").
        Status::Downloading {
            done: 0,
            total: None,
        } => "Downloading".to_owned(),
        Status::Downloading { done, total: None } => {
            format!("Downloading · {}", format_bytes(*done))
        }
        Status::Downloading {
            done,
            total: Some(total),
        } => format!(
            "Downloading · {} of {}",
            format_bytes(*done),
            format_bytes(*total)
        ),
        Status::Retrying {
            at,
            attempts,
            error,
        } => {
            let when = match at.map(|at| at - now).filter(|left| *left > 0) {
                Some(left) => format!("Retrying in {}", time_label(left)),
                None => "Retrying now".to_owned(),
            };
            let error = error
                .as_deref()
                .map_or(String::new(), |error| format!(" · {error}"));
            format!("{when}{error} (attempt {} of {MAX_ATTEMPTS})", attempts + 1)
        }
        Status::Failed { error: None } => "Failed".to_owned(),
        Status::Failed { error: Some(error) } => format!("Failed · {error}"),
        Status::Downloaded { leaving_at: None } => "Downloaded".to_owned(),
        Status::Downloaded {
            leaving_at: Some(at),
        } if *at > now => format!("Downloaded · removed in {}", time_label(at - now)),
        Status::Downloaded {
            leaving_at: Some(_),
        } => "Downloaded · removing".to_owned(),
    }
}

/// `45 s`, `3 min`, `20 h`, rounded up, for a positive span in ms.
fn time_label(ms: i64) -> String {
    let seconds = u64::try_from(ms).unwrap_or(0).div_ceil(1000);
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 60 * 60 {
        format!("{} min", seconds.div_ceil(60))
    } else {
        format!("{} h", seconds.div_ceil(60 * 60))
    }
}

/// The source column: what keeps the episode on this device.
pub fn source_label(entry: &Entry) -> &'static str {
    match (entry.pinned, entry.auto) {
        (true, _) => "Pinned",
        (false, true) => "Auto",
        (false, false) => "–",
    }
}

/// The size column. `~` marks the feed's own claim.
pub fn size_label(entry: &Entry) -> String {
    match entry.size {
        Some((bytes, true)) => format_bytes(bytes),
        Some((bytes, false)) => format!("~{}", format_bytes(bytes)),
        None => "–".to_owned(),
    }
}

// ==============================================================================
// Loading
// ==============================================================================

/// The tables [`load`] reads.
pub fn tables() -> TableSet {
    [
        Table::Podcast,
        Table::Episode,
        Table::Download,
        Table::DownloadWish,
        Table::KnownSize,
        Table::DeviceSetting,
    ]
    .into_iter()
    .collect()
}

/// Every episode a download row or a wish names, newest first.
pub fn load(connection: &Connection) -> Result<Vec<Entry>> {
    let grace_ms = crate::reconcile::load_settings(connection)
        .context("read the device settings")?
        .grace_ms;
    connection
        .prepare(
            "with ids as (select episode_id from download
                          union select episode_id from download_wish)
             select e.id, e.podcast_id, e.title, coalesce(p.title, p.local_title, ''),
                 exists(select 1 from download_wish w
                        where w.episode_id = e.id and w.source = 'pin'),
                 exists(select 1 from download_wish w
                        where w.episode_id = e.id and w.source = 'auto'),
                 d.state, d.wait_reason, d.attempts, d.next_attempt_at, d.last_error,
                 d.bytes_done, d.bytes_total, d.unwanted_since,
                 coalesce(case when d.state = 'complete' then d.bytes_done end,
                          d.bytes_total, ks.bytes),
                 e.audio_bytes
             from ids join episode e on e.id = ids.episode_id
             left join podcast p on p.id = e.podcast_id
             left join download d on d.episode_id = e.id
             left join known_size ks on ks.episode_id = e.id
             order by e.published_at desc, e.id desc",
        )
        .context("prepare the downloads query")?
        .query_map([], |row| {
            let state: Option<String> = row.get(6)?;
            let wait_reason: Option<String> = row.get(7)?;
            let bytes = |index| -> rusqlite::Result<Option<u64>> {
                Ok(row
                    .get::<_, Option<i64>>(index)?
                    .and_then(|bytes| u64::try_from(bytes).ok()))
            };
            let exact = bytes(14)?.map(|bytes| (bytes, true));
            let row_state = match state {
                // An unknown state reads as failed, as in the reconciler.
                Some(state) => Some(Row {
                    state: State::parse(&state).unwrap_or(State::Failed),
                    wait_reason: wait_reason.as_deref().and_then(WaitReason::parse),
                    attempts: row.get::<_, Option<u32>>(8)?.unwrap_or(0),
                    next_attempt_at: row.get(9)?,
                    last_error: row.get(10)?,
                    bytes_done: bytes(11)?.unwrap_or(0),
                    bytes_total: bytes(12)?,
                    leaving_at: row
                        .get::<_, Option<i64>>(13)?
                        .filter(|_| state == "complete")
                        .map(|since| since.saturating_add(grace_ms)),
                }),
                None => None,
            };
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, bool>(5)?,
                row_state,
                exact.or(bytes(15)?.map(|bytes| (bytes, false))),
            ))
        })
        .context("query the downloads")?
        .map(|row| {
            let (episode_id, podcast_id, title, podcast_title, pinned, auto, row, size) =
                row.context("read a download")?;
            Ok(Entry {
                episode_id: episode_id.parse().context("parse an episode id")?,
                podcast_id: podcast_id.parse().context("parse a podcast id")?,
                title,
                podcast_title,
                pinned,
                auto,
                row,
                size,
            })
        })
        .collect()
}

// ==============================================================================
// Writes
// ==============================================================================

/// What [`remove`] took away from an auto download, for its undo.
#[derive(Debug, Clone, PartialEq)]
pub struct Removed {
    pub episode_id: EpisodeId,
    pub podcast_id: PodcastId,
    /// `(source, created_at, metered_ok)` of each deleted wish.
    wishes: Vec<(String, i64, bool)>,
}

/// Does `act` for an episode. Returns what to offer an undo for when the
/// episode was archived ("Archived and removed", spec 7.4).
pub async fn perform(
    store: &Store,
    sync: &SyncHandle,
    episode_id: EpisodeId,
    podcast_id: PodcastId,
    act: Act,
) -> Result<Option<Removed>> {
    match act {
        Act::Cancel | Act::Remove | Act::Archive => remove(store, sync, episode_id, podcast_id)
            .await
            .context("remove the download"),
        Act::Retry => {
            retry(store, episode_id)
                .await
                .context("retry the download")?;
            Ok(None)
        }
        Act::UseMetered => {
            pin(store, episode_id, true)
                .await
                .context("pin for the metered network")?;
            Ok(None)
        }
        Act::Download | Act::DownloadAnyway | Act::Keep => {
            pin(store, episode_id, false)
                .await
                .context("pin the episode")?;
            Ok(None)
        }
    }
}

/// Pins episode `id` (spec 7.1). `metered_ok` also lets that one pin use a
/// metered network; an existing pin keeps its `created_at`, so a finished
/// pin's release (spec 7.4) still dates from when it was made.
pub async fn pin(store: &Store, id: EpisodeId, metered_ok: bool) -> Result<()> {
    let now = Timestamp::now().as_epoch_ms();
    store
        .write(move |transaction| {
            transaction
                .execute(
                    "insert into download_wish (episode_id, source, created_at, metered_ok)
                     values (?1, 'pin', ?2, ?3)
                     on conflict (episode_id, source) do update
                         set metered_ok = max(metered_ok, excluded.metered_ok)",
                    params![id.to_string(), now, metered_ok],
                )
                .context("upsert the pin")?;
            Ok(())
        })
        .await
        .context("write the pin")
}

/// A failed row back to `queued` with its attempts reset (spec 7.2).
async fn retry(store: &Store, id: EpisodeId) -> Result<()> {
    store
        .write(move |transaction| {
            transaction
                .execute(
                    "update download set state = 'queued', attempts = 0, last_error = null,
                         next_attempt_at = null, wait_reason = null
                     where episode_id = ?1 and state = 'failed'",
                    [id.to_string()],
                )
                .context("requeue the row")?;
            Ok(())
        })
        .await
        .context("write the retry")
}

/// Drops every wish for the episode, and with them the row. A finished
/// file gets `unwanted_since = 0`, so the next pass deletes it instead of
/// keeping it for the grace period. When an auto wish covered the episode,
/// it is archived in the same transaction, or the next pass would wish it
/// again (spec 7.4); a pin alongside goes too, since the user asked for the
/// download to stop.
async fn remove(
    store: &Store,
    sync: &SyncHandle,
    episode_id: EpisodeId,
    podcast_id: PodcastId,
) -> Result<Option<Removed>> {
    let (removed_tx, removed_rx) = std::sync::mpsc::channel();
    local_write::write_all(store, sync, move |transaction| {
        let id = episode_id.to_string();
        let wishes = transaction
            .prepare(
                "select source, created_at, metered_ok from download_wish where episode_id = ?1",
            )
            .context("prepare the wish query")?
            .query_map([&id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .context("query the wishes")?
            .collect::<rusqlite::Result<Vec<(String, i64, bool)>>>()
            .context("read the wishes")?;
        transaction
            .execute("delete from download_wish where episode_id = ?1", [&id])
            .context("delete the wishes")?;
        transaction
            .execute(
                "update download set unwanted_since = 0
                 where episode_id = ?1 and state = 'complete'",
                [&id],
            )
            .context("release the finished file")?;
        let auto = wishes
            .iter()
            .any(|(source, _, _)| source == Source::Auto.as_str());
        if !auto {
            return Ok(Vec::new());
        }
        // The receiver outlives the write.
        let _ = removed_tx.send(Removed {
            episode_id,
            podcast_id,
            wishes,
        });
        Ok(vec![LocalChange::Episode {
            episode_id,
            podcast_id,
            change: EpisodeChange::Archived(true),
        }])
    })
    .await
    .context("write the removal")?;
    Ok(removed_rx.try_recv().ok())
}

/// Undoes "Archived and removed": unarchives the episode and puts its
/// wishes back, so the reconciler downloads it again.
pub async fn undo(store: &Store, sync: &SyncHandle, removed: Removed) -> Result<()> {
    local_write::write_all(store, sync, move |transaction| {
        for (source, created_at, metered_ok) in &removed.wishes {
            transaction
                .execute(
                    "insert into download_wish (episode_id, source, created_at, metered_ok)
                     values (?1, ?2, ?3, ?4) on conflict do nothing",
                    params![
                        removed.episode_id.to_string(),
                        source,
                        created_at,
                        metered_ok
                    ],
                )
                .context("restore a wish")?;
        }
        Ok(vec![LocalChange::Episode {
            episode_id: removed.episode_id,
            podcast_id: removed.podcast_id,
            change: EpisodeChange::Archived(false),
        }])
    })
    .await
    .context("write the undo")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const NOW: i64 = 1_000_000_000;
    const EPISODE: &str = "e0000000-0000-5000-8000-000000000001";

    fn entry(row: Option<Row>) -> Entry {
        Entry {
            episode_id: EPISODE.parse().expect("a UUID"),
            podcast_id: "a1000000-0000-5000-8000-000000000001"
                .parse()
                .expect("a UUID"),
            title: "The chip wars".to_owned(),
            podcast_title: "Hard Fork".to_owned(),
            pinned: false,
            auto: true,
            row,
            size: None,
        }
    }

    fn row(state: State) -> Row {
        Row {
            state,
            wait_reason: None,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            bytes_done: 0,
            bytes_total: None,
            leaving_at: None,
        }
    }

    /// `(label, button labels)` of `entry` at [`NOW`].
    fn shown(
        entry: &Entry,
        fit: &Fit,
        progress: &HashMap<String, Progress>,
    ) -> (String, Vec<&'static str>) {
        let status = status(entry, fit, progress);
        (
            status_label(&status, NOW),
            actions(&status).iter().map(|act| act.label()).collect(),
        )
    }

    #[test]
    fn every_state_has_its_label_and_a_way_out() {
        let none = HashMap::new();
        let fit = Fit::default();
        let waiting = |reason| {
            entry(Some(Row {
                wait_reason: Some(reason),
                ..row(State::Queued)
            }))
        };
        let cases: Vec<(Entry, &str, Vec<&str>)> = vec![
            (entry(None), "Queued", vec!["Cancel"]),
            (entry(Some(row(State::Queued))), "Queued", vec!["Cancel"]),
            (
                waiting(WaitReason::Connection),
                "Waiting · no connection",
                vec!["Cancel"],
            ),
            (
                waiting(WaitReason::Wifi),
                "Waiting · metered network",
                vec!["Use metered network", "Cancel"],
            ),
            (
                waiting(WaitReason::Space),
                "Paused · storage full",
                vec!["Cancel"],
            ),
            (
                waiting(WaitReason::Budget),
                "Waiting · doesn't fit the budget",
                vec!["Download anyway", "Cancel"],
            ),
            (
                entry(Some(row(State::Transferring))),
                "Downloading",
                vec!["Cancel"],
            ),
            (
                entry(Some(Row {
                    bytes_done: 31_000_000,
                    bytes_total: Some(50_000_000),
                    ..row(State::Transferring)
                })),
                "Downloading · 31.0 MB of 50.0 MB",
                vec!["Cancel"],
            ),
            (
                entry(Some(Row {
                    attempts: 2,
                    next_attempt_at: Some(NOW + 90_000),
                    last_error: Some("connection reset".to_owned()),
                    ..row(State::WaitingRetry)
                })),
                "Retrying in 2 min · connection reset (attempt 3 of 8)",
                vec!["Cancel"],
            ),
            (
                entry(Some(Row {
                    attempts: 1,
                    next_attempt_at: Some(NOW - 1),
                    ..row(State::WaitingRetry)
                })),
                "Retrying now (attempt 2 of 8)",
                vec!["Cancel"],
            ),
            (
                entry(Some(Row {
                    last_error: Some("HTTP 404 Not Found".to_owned()),
                    ..row(State::Failed)
                })),
                "Failed · HTTP 404 Not Found",
                vec!["Retry", "Remove"],
            ),
            (
                entry(Some(row(State::Failed))),
                "Failed",
                vec!["Retry", "Remove"],
            ),
            (
                entry(Some(row(State::Complete))),
                "Downloaded",
                vec!["Remove"],
            ),
            (
                entry(Some(Row {
                    leaving_at: Some(NOW + 20 * 3_600_000),
                    ..row(State::Complete)
                })),
                "Downloaded · removed in 20 h",
                vec!["Keep", "Remove"],
            ),
            (
                entry(Some(Row {
                    leaving_at: Some(NOW - 1),
                    ..row(State::Complete)
                })),
                "Downloaded · removing",
                vec!["Keep", "Remove"],
            ),
        ];
        for (entry, label, buttons) in cases {
            let (shown_label, shown_buttons) = shown(&entry, &fit, &none);
            assert_eq!(shown_label, label, "{:?}", entry.row);
            assert_eq!(shown_buttons, buttons, "{label}");
        }
    }

    #[test]
    fn a_wish_the_budget_left_out_reads_skipped() {
        let fit = Fit {
            skipped: BTreeSet::from([EPISODE.to_owned()]),
            pins_over_budget: false,
        };
        assert_eq!(
            shown(&entry(None), &fit, &HashMap::new()),
            (
                "Skipped · doesn't fit the budget".to_owned(),
                vec!["Download anyway", "Archive"]
            )
        );
    }

    #[test]
    fn live_progress_overrides_the_checkpoint() {
        let entry = entry(Some(Row {
            bytes_done: 1_000_000,
            ..row(State::Transferring)
        }));
        let progress = HashMap::from([(
            EPISODE.to_owned(),
            Progress {
                bytes_done: 2_500_000,
                bytes_total: Some(10_000_000),
            },
        )]);
        assert_eq!(
            shown(&entry, &Fit::default(), &progress).0,
            "Downloading · 2.5 MB of 10.0 MB"
        );
    }

    #[test]
    fn sources_and_sizes_read_as_in_d4() {
        let mut entry = entry(None);
        assert_eq!(source_label(&entry), "Auto");
        entry.pinned = true;
        assert_eq!(source_label(&entry), "Pinned");
        entry.pinned = false;
        entry.auto = false;
        assert_eq!(source_label(&entry), "–");

        assert_eq!(size_label(&entry), "–");
        entry.size = Some((38_000_000, false));
        assert_eq!(size_label(&entry), "~38.0 MB");
        entry.size = Some((50_000_000, true));
        assert_eq!(size_label(&entry), "50.0 MB");
    }
}
