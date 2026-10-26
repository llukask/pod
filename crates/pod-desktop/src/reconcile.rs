//! The download reconciler (spec 7.4): decides which episodes should be on
//! disk and starts, pauses, cancels and deletes transfers to match.
//!
//! A pass is three steps:
//!
//! 1. [`load`] reads the download tables into a [`plan::Snapshot`] in one
//!    read transaction.
//! 2. [`plan::plan`] turns the snapshot and the [`plan::Conditions`] (clock,
//!    network, free space, the playing episode, live transfers) into
//!    [`Action`]s. It is pure, and `conformance/downloads.json` drives it.
//! 3. [`apply`] writes the actions in one transaction, every row write
//!    guarded by the `run_id` the snapshot saw. The executor then does the
//!    file and job side: cancelling, deleting a run's files, renaming a
//!    resumable `.part` to the new run's name and calling
//!    [`Downloader::start`].
//!
//! The pass then warms the artwork cache (spec 7.5) the same way, through
//! [`artwork::load`], [`artwork::plan`] and a fetch batch, unless the last
//! batch is still running.
//!
//! A pass that finds nothing to change writes nothing, so the table events
//! its own writes cause wake the loop for one more pass that ends there.
//! Passes take the job map's mutex, so only one runs at a time, and
//! [`Reconciler::cancel_all`] holds it to keep passes out during logout.

pub mod artwork;
pub mod plan;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use pod_protocol::{EpisodeId, Timestamp};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::download::{Downloader, Progress, RunFiles, WaitReason};
use crate::net::NetworkState;
use crate::store::{Store, Table, TableSet};
use crate::sync::engine::SyncEngine;
use plan::{Action, Conditions, Download, Episode, PodcastRule, RowRef, Settings, Snapshot, Wish};

/// The longest a pass waits for a deadline (a retry, the end of a grace
/// period). A sleeping laptop's monotonic clock stops, so a far deadline
/// could fire late by the length of the sleep; this bounds that (spec 7.4's
/// "every 15 minutes while any row is `waiting_retry`").
const MAX_SLEEP: Duration = Duration::from_secs(15 * 60);
/// How long the loop waits after a trigger, so a burst (a sync touching
/// several tables, two transfers ending together) shares one pass.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// After a failed pass, try again this soon even without a trigger.
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(60);

// ==============================================================================
// Reading and writing the tables
// ==============================================================================

/// Reads everything [`plan::plan`] needs in one snapshot.
pub fn load(connection: &Connection) -> Result<Snapshot> {
    let podcasts = connection
        .prepare("select podcast_id, auto_keep, auto_on_metered from podcast_device")
        .context("prepare the podcast_device query")?
        .query_map([], |row| {
            Ok(PodcastRule {
                podcast_id: row.get(0)?,
                auto_keep: row.get::<_, i64>(1)?.clamp(0, i64::from(u32::MAX)) as u32,
                auto_on_metered: row.get(2)?,
            })
        })
        .context("query podcast_device")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read podcast_device")?;

    let episodes = connection
        .prepare(
            "select e.id, e.podcast_id, e.published_at, e.audio_bytes, e.audio_url,
                 coalesce(st.played, 0), st.played_at, coalesce(st.archived, 0)
             from episode e left join episode_state st on st.episode_id = e.id
             where e.podcast_id in (select podcast_id from podcast_device where auto_keep > 0)
                or e.id in (select episode_id from download_wish)
                or e.id in (select episode_id from download)",
        )
        .context("prepare the episode query")?
        .query_map([], |row| {
            Ok(Episode {
                id: row.get(0)?,
                podcast_id: row.get(1)?,
                published_at: row.get(2)?,
                audio_bytes: row
                    .get::<_, Option<i64>>(3)?
                    .and_then(|bytes| u64::try_from(bytes).ok()),
                audio_url: row.get(4)?,
                played: row.get(5)?,
                played_at: row.get(6)?,
                archived: row.get(7)?,
            })
        })
        .context("query episodes")?
        .map(|episode| episode.map(|episode| (episode.id.clone(), episode)))
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .context("read episodes")?;

    let wishes = connection
        .prepare("select episode_id, source, created_at, metered_ok from download_wish")
        .context("prepare the wish query")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .context("query download_wish")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read download_wish")?
        .into_iter()
        // The table's check constraint admits only the two sources.
        .filter_map(|(episode_id, source, created_at, metered_ok)| {
            Some(Wish {
                source: plan::Source::parse(&source)?,
                episode_id,
                created_at,
                metered_ok,
            })
        })
        .collect();

    let downloads = connection
        .prepare(
            "select episode_id, state, audio_url, bytes_done, next_attempt_at, wait_reason,
                 unwanted_since, run_id
             from download",
        )
        .context("prepare the download query")?
        .query_map([], |row| {
            let state: String = row.get(1)?;
            let wait_reason: Option<String> = row.get(5)?;
            Ok(Download {
                episode_id: row.get(0)?,
                // A state this build doesn't know is treated like `failed`:
                // kept while wanted, deleted when not.
                state: plan::State::parse(&state).unwrap_or(plan::State::Failed),
                audio_url: row.get(2)?,
                bytes_done: row.get::<_, i64>(3)?.try_into().unwrap_or(0),
                next_attempt_at: row.get(4)?,
                wait_reason: wait_reason.as_deref().and_then(WaitReason::parse),
                unwanted_since: row.get(6)?,
                run_id: row.get(7)?,
            })
        })
        .context("query download")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read download")?;

    let known_sizes = connection
        .prepare("select episode_id, bytes from known_size")
        .context("prepare the known_size query")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.try_into().unwrap_or(0),
            ))
        })
        .context("query known_size")?
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .context("read known_size")?;

    Ok(Snapshot {
        podcasts,
        episodes,
        wishes,
        downloads,
        known_sizes,
        settings: load_settings(connection).context("read the device settings")?,
    })
}

/// The device settings of spec 7.1. Values are JSON; one that is missing
/// or doesn't parse as the right type reads as the default.
pub(crate) fn load_settings(connection: &Connection) -> Result<Settings> {
    let value = |key: &str| -> Result<Option<serde_json::Value>> {
        let text: Option<String> = connection
            .query_row(
                "select value from device_setting where key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("read device setting {key}"))?;
        Ok(text.and_then(|text| serde_json::from_str(&text).ok()))
    };
    let defaults = Settings::default();
    Ok(Settings {
        budget: value(plan::DOWNLOAD_BUDGET_BYTES)?
            .and_then(|value| value.as_u64())
            .unwrap_or(defaults.budget),
        on_metered: value(plan::DOWNLOAD_ON_METERED)?
            .and_then(|value| value.as_bool())
            .unwrap_or(defaults.on_metered),
        grace_ms: value(plan::EVICT_GRACE_HOURS)?
            .and_then(|value| value.as_f64())
            .filter(|hours| hours.is_finite() && *hours >= 0.0)
            .map_or(defaults.grace_ms, plan::hours_to_ms),
    })
}

/// What [`apply`] leaves for the executor: work outside the database, only
/// for actions whose guard matched.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// `(episode_id, previous run, new run)` to start.
    pub starts: Vec<(String, Option<String>, String)>,
    /// `(episode_id, run)` whose files to delete.
    pub dead_runs: Vec<(String, String)>,
    /// Episodes whose live transfer to cancel.
    pub cancels: Vec<String>,
}

/// Writes `actions` in order inside `transaction`. `new_run` names each
/// started run.
pub fn apply(
    transaction: &Transaction<'_>,
    actions: &[Action],
    mut new_run: impl FnMut() -> String,
) -> Result<Applied> {
    let mut applied = Applied::default();
    // Runs `sql` with the row's id and run as `?1` and `?2` and the extra
    // parameters after them, and says whether it matched.
    let guarded = |row: &RowRef, sql: &str, extra: &[&dyn rusqlite::ToSql]| -> Result<bool> {
        let mut parameters: Vec<&dyn rusqlite::ToSql> = vec![&row.episode_id, &row.run_id];
        parameters.extend_from_slice(extra);
        let changed = transaction
            .execute(sql, parameters.as_slice())
            .with_context(|| format!("run {sql:?}"))?;
        Ok(changed > 0)
    };
    for action in actions {
        match action {
            Action::AddWish {
                episode_id,
                source,
                created_at,
            } => {
                transaction
                    .execute(
                        "insert into download_wish (episode_id, source, created_at)
                         values (?1, ?2, ?3) on conflict do nothing",
                        params![episode_id, source.as_str(), created_at],
                    )
                    .context("add a wish")?;
            }
            Action::RemoveWish { episode_id, source } => {
                transaction
                    .execute(
                        "delete from download_wish where episode_id = ?1 and source = ?2",
                        params![episode_id, source.as_str()],
                    )
                    .context("remove a wish")?;
            }
            Action::Insert {
                episode_id,
                audio_url,
            } => {
                transaction
                    .execute(
                        "insert into download (episode_id, state, audio_url, path)
                         values (?1, 'queued', ?2, '') on conflict do nothing",
                        params![episode_id, audio_url],
                    )
                    .context("insert a queued row")?;
            }
            Action::SetUnwanted { row, since } => {
                guarded(
                    row,
                    "update download set unwanted_since = ?3
                     where episode_id = ?1 and run_id is ?2",
                    &[since],
                )?;
            }
            Action::SetWait { row, reason } => {
                guarded(
                    row,
                    "update download set wait_reason = ?3
                     where episode_id = ?1 and run_id is ?2 and state = 'queued'",
                    &[&reason.map(WaitReason::as_str)],
                )?;
            }
            Action::Requeue { row } => {
                guarded(
                    row,
                    "update download set state = 'queued', next_attempt_at = null,
                         wait_reason = null
                     where episode_id = ?1 and run_id is ?2
                         and state in ('waiting_retry', 'transferring')",
                    &[],
                )?;
            }
            Action::Reset { row, audio_url } => {
                // The old run's writes now miss, so a live transfer stops
                // at its next write and deletes its own `.part`.
                let matched = guarded(
                    row,
                    "update download set state = 'queued', audio_url = ?3, run_id = null,
                         path = '', bytes_total = null, bytes_done = 0, validator = null,
                         attempts = 0, next_attempt_at = null, last_error = null,
                         wait_reason = null, completed_at = null, unwanted_since = null
                     where episode_id = ?1 and run_id is ?2",
                    &[audio_url],
                )?;
                if matched {
                    applied.cancels.push(row.episode_id.clone());
                    applied
                        .dead_runs
                        .extend(row.run_id.clone().map(|run| (row.episode_id.clone(), run)));
                }
            }
            Action::Pause { row, reason } => {
                let matched = guarded(
                    row,
                    "update download set state = 'queued', wait_reason = ?3
                     where episode_id = ?1 and run_id is ?2 and state = 'transferring'",
                    &[&reason.as_str()],
                )?;
                if matched {
                    applied.cancels.push(row.episode_id.clone());
                }
            }
            Action::Remove { row } => {
                let matched = guarded(
                    row,
                    "delete from download where episode_id = ?1 and run_id is ?2",
                    &[],
                )?;
                if matched {
                    applied.cancels.push(row.episode_id.clone());
                    applied
                        .dead_runs
                        .extend(row.run_id.clone().map(|run| (row.episode_id.clone(), run)));
                }
            }
            Action::Start { row } => {
                let run = new_run();
                let matched = guarded(
                    row,
                    "update download set state = 'transferring', run_id = ?3,
                         wait_reason = null, next_attempt_at = null
                     where episode_id = ?1 and run_id is ?2 and state = 'queued'",
                    &[&run],
                )?;
                if matched {
                    applied
                        .starts
                        .push((row.episode_id.clone(), row.run_id.clone(), run));
                }
            }
        }
    }
    Ok(applied)
}

// ==============================================================================
// Files
// ==============================================================================

/// The files of one run, `.part` or finished, in any podcast directory.
/// Both names start with `<episode_id>.<run_id>.` (spec 7.1), and run ids
/// are unique, so the prefix can't match another run. Looking in every
/// directory finds them even when the episode row, and with it the podcast
/// id, is already gone.
fn run_files(root: &Path, episode_id: &str, run_id: &str) -> Vec<PathBuf> {
    let prefix = format!("{episode_id}.{run_id}.");
    let Ok(directories) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    directories
        .flatten()
        .filter_map(|directory| std::fs::read_dir(directory.path()).ok())
        .flat_map(|files| files.flatten())
        .filter(|file| file.file_name().to_string_lossy().starts_with(&prefix))
        .map(|file| file.path())
        .collect()
}

fn remove_file(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        // The startup repair deletes files no row accounts for.
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "can't delete a download file")
        }
    }
}

/// The file side of a pass: deletes dead runs' files and gives each started
/// run the previous run's `.part`, so it resumes from it (spec 7.2,
/// `queued` → `transferring`). The previous run's job is dead: a pass only
/// starts rows without a live job.
fn move_files(root: &Path, applied: &Applied) {
    for (episode_id, run) in &applied.dead_runs {
        for file in run_files(root, episode_id, run) {
            remove_file(&file);
        }
    }
    for (episode_id, previous, run) in &applied.starts {
        let Some(previous) = previous else { continue };
        let old_prefix = format!("{episode_id}.{previous}.");
        for file in run_files(root, episode_id, previous) {
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            let Some(rest) = name
                .as_deref()
                .and_then(|name| name.strip_prefix(&old_prefix))
            else {
                continue;
            };
            if !rest.ends_with(".part") {
                continue;
            }
            let renamed = file.with_file_name(format!("{episode_id}.{run}.{rest}"));
            if let Err(error) = std::fs::rename(&file, &renamed) {
                // The transfer then starts from zero, and the startup
                // repair deletes the old file.
                tracing::warn!(path = %file.display(), %error, "can't hand a .part to the next run");
            }
        }
    }
}

/// Bytes available to this user on the file system holding `path`.
pub fn free_space(path: &Path) -> Result<u64> {
    let stats = rustix::fs::statvfs(path).context("statvfs the downloads root")?;
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

// ==============================================================================
// The reconciler
// ==============================================================================

/// A transfer the reconciler started.
#[derive(Debug)]
struct Job {
    cancel: CancellationToken,
    /// Ends after the transfer, once it has woken the loop.
    task: JoinHandle<()>,
}

/// The running jobs: transfers by episode id, and at most one artwork fetch
/// batch.
#[derive(Debug, Default)]
struct Jobs {
    transfers: HashMap<String, Job>,
    artwork: Option<Job>,
}

/// The reconciler. Cloning is cheap and every clone shares one job map.
/// Registered as a GPUI global.
#[derive(Clone, Debug)]
pub struct Reconciler {
    inner: Arc<Inner>,
}

impl gpui_kit::Global for Reconciler {}

#[derive(Debug)]
struct Inner {
    store: Store,
    downloader: Downloader,
    network: watch::Receiver<NetworkState>,
    playing: watch::Sender<Option<EpisodeId>>,
    fit: watch::Sender<plan::Fit>,
    /// Byte progress of the running transfers, by episode id. Kept in
    /// memory only (spec 7.2).
    progress: watch::Sender<HashMap<String, Progress>>,
    /// Asks for a pass after a write the table subscription leaves out
    /// (the downloads page's writes to `download`).
    woken: Arc<Notify>,
    /// The live transfers. Its mutex is the one-pass-at-a-time lock.
    jobs: Arc<Mutex<Jobs>>,
    /// Woken when a transfer or an artwork batch ends.
    transfer_ended: Arc<Notify>,
    artwork: artwork::Fetcher,
    /// Stubbed in tests.
    free_space: fn(&Path) -> Result<u64>,
}

/// Returned by [`Reconciler::cancel_all`]. No pass runs, and so no transfer
/// starts, until it is dropped.
#[derive(Debug)]
pub struct DownloadsStopped {
    _jobs: OwnedMutexGuard<Jobs>,
}

impl Reconciler {
    /// `artwork_root` is `<data_dir>/artwork` in production.
    pub fn new(
        downloader: Downloader,
        artwork_root: PathBuf,
        free_space: fn(&Path) -> Result<u64>,
    ) -> Reconciler {
        Reconciler {
            inner: Arc::new(Inner {
                artwork: artwork::Fetcher {
                    store: downloader.store.clone(),
                    client: downloader.client.clone(),
                    root: artwork_root,
                    retry_at: Arc::default(),
                },
                store: downloader.store.clone(),
                network: downloader.network.clone(),
                downloader,
                playing: watch::channel(None).0,
                fit: watch::channel(plan::Fit::default()).0,
                progress: watch::channel(HashMap::new()).0,
                woken: Arc::default(),
                jobs: Arc::default(),
                transfer_ended: Arc::default(),
                free_space,
            }),
        }
    }

    /// Tells the reconciler which episode is playing, so its file is kept
    /// (spec 7.4). Every change triggers a pass, so `None` on stop releases
    /// the file to its grace period: that is the "playback stop" trigger
    /// of spec 7.4. The player calls it on start, episode change and stop.
    pub fn set_playing(&self, episode: Option<EpisodeId>) {
        self.inner.playing.send_if_modified(|playing| {
            let changed = *playing != episode;
            *playing = episode;
            changed
        });
    }

    /// What the last pass's budget fitting left out, for the downloads
    /// page.
    pub fn fit(&self) -> watch::Receiver<plan::Fit> {
        self.inner.fit.subscribe()
    }

    /// Stands in for a pass's budget fitting in UI tests.
    #[cfg(test)]
    pub fn show_fit(&self, fit: plan::Fit) {
        self.inner.fit.send_replace(fit);
    }

    /// The running transfers' byte progress, by episode id. An entry goes
    /// when its transfer ends.
    pub fn progress(&self) -> watch::Receiver<HashMap<String, Progress>> {
        self.inner.progress.subscribe()
    }

    /// Runs a pass soon. For writes to `download`, which the loop doesn't
    /// subscribe to.
    pub fn wake(&self) {
        self.inner.woken.notify_one();
    }

    /// The downloads root, `<data_dir>/audio` in production.
    pub fn root(&self) -> &Path {
        &self.inner.downloader.root
    }

    /// The artwork cache's directory (spec 7.5). Views load images from
    /// here by the `artwork.file` names.
    pub fn artwork_root(&self) -> &Path {
        &self.inner.artwork.root
    }

    /// Cancels every transfer and the artwork batch, and waits up to `timeout` for them to stop
    /// (logout, spec 5.7). The returned guard keeps passes from running, and
    /// so new transfers from starting, until it is dropped. Rows stay
    /// `transferring`; a pass after the guard drops requeues any that are
    /// still there.
    pub async fn cancel_all(&self, timeout: Duration) -> DownloadsStopped {
        let mut jobs = self.inner.jobs.clone().lock_owned().await;
        let artwork = jobs.artwork.take();
        let jobs_running = jobs.transfers.drain().map(|(_, job)| job).chain(artwork);
        let tasks: Vec<JoinHandle<()>> = jobs_running
            .map(|job| {
                job.cancel.cancel();
                job.task
            })
            .collect();
        // A transfer that outlives the wait finds its row gone at its next
        // write and deletes its own files (spec 5.7).
        let _ = tokio::time::timeout(timeout, futures_util::future::join_all(tasks)).await;
        DownloadsStopped { _jobs: jobs }
    }

    /// Repairs crash leftovers, then runs a pass on every trigger of spec
    /// 7.4 until the store closes.
    pub async fn run(self, sync: SyncEngine) {
        if let Err(error) = self.repair().await {
            tracing::error!(
                error = format!("{error:#}"),
                "download startup repair failed"
            );
        }
        // Wish changes, auto-download settings, played and archived flags
        // (a local write marks an episode played without a sync), and the
        // device settings. Not `download`: transfers checkpoint into it
        // every few seconds, and a transfer's end wakes the loop itself.
        // TODO: position writes during playback touch `episode_state` every
        // 5 s and cost a pass each; filter on the played and archived
        // columns if that shows up.
        let mut tables = self.inner.store.subscribe(TableSet::from_iter([
            Table::DownloadWish,
            Table::PodcastDevice,
            Table::EpisodeState,
            Table::DeviceSetting,
        ]));
        let mut network = self.inner.network.clone();
        let mut network_open = true;
        let mut playing = self.inner.playing.subscribe();
        loop {
            let wake = match self.pass().await {
                Ok(wake_at) => wake_at.map(|wake_at| {
                    let delay = wake_at.saturating_sub(Timestamp::now().as_epoch_ms());
                    Duration::from_millis(delay.try_into().unwrap_or(0)).min(MAX_SLEEP)
                }),
                Err(error) => {
                    tracing::error!(error = format!("{error:#}"), "download pass failed");
                    Some(RETRY_AFTER_ERROR)
                }
            };
            let deadline = wake.map(|delay| Instant::now() + delay);
            tokio::select! {
                () = sync.loop_ended().notified() => {}
                changed = tables.changed() => {
                    if changed.is_none() {
                        return;
                    }
                }
                changed = network.changed(), if network_open => {
                    network_open = changed.is_ok();
                }
                _ = playing.changed() => {}
                () = self.inner.transfer_ended.notified() => {}
                () = self.inner.woken.notified() => {}
                () = sleep_until(deadline), if deadline.is_some() => {}
            }
            tokio::time::sleep(DEBOUNCE).await;
            tables.try_changed();
        }
    }

    /// One pass. Returns when the next one is due without a trigger.
    pub(crate) async fn pass(&self) -> Result<Option<i64>> {
        let mut jobs = self.inner.jobs.lock().await;
        jobs.transfers.retain(|_, job| !job.task.is_finished());

        let root = self.inner.downloader.root.clone();
        let free_space = self.inner.free_space;
        let free_bytes = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&root).context("create the downloads root")?;
            free_space(&root)
        })
        .await
        .context("wait for the free space check")?
        .context("check free space")?;
        let conditions = Conditions {
            now: Timestamp::now().as_epoch_ms(),
            network: *self.inner.network.borrow(),
            free_bytes,
            playing: self.inner.playing.borrow().map(|id| id.to_string()),
            live: jobs.transfers.keys().cloned().collect(),
        };
        let snapshot = self
            .inner
            .store
            .read(load)
            .await
            .context("load the download tables")?;
        let plan = plan::plan(&snapshot, &conditions);
        tracing::debug!(?plan, "download pass");

        if !plan.actions.is_empty() {
            let actions = plan.actions;
            let applied = self
                .inner
                .store
                .write(move |transaction| {
                    apply(transaction, &actions, || uuid::Uuid::now_v7().to_string())
                })
                .await
                .context("apply the download plan")?;
            for episode_id in &applied.cancels {
                if let Some(job) = jobs.transfers.get(episode_id) {
                    job.cancel.cancel();
                }
            }
            let root = self.inner.downloader.root.clone();
            let applied = tokio::task::spawn_blocking(move || {
                move_files(&root, &applied);
                applied
            })
            .await
            .context("wait for the file moves")?;
            for (episode_id, _, run) in applied.starts {
                let job = self.start(episode_id.clone(), run);
                jobs.transfers.insert(episode_id, job);
            }
        }
        // A running batch already holds the plan's fetches, and its end
        // wakes the loop for the next look.
        if jobs
            .artwork
            .as_ref()
            .is_none_or(|job| job.task.is_finished())
        {
            jobs.artwork = self
                .artwork_pass(conditions.now, conditions.network)
                .await
                .context("run the artwork pass")?;
        }
        self.inner.fit.send_if_modified(|fit| {
            let changed = *fit != plan.fit;
            *fit = plan.fit;
            changed
        });
        Ok(plan.wake_at)
    }

    /// The artwork step (spec 7.5): evicts and touches in one write, then
    /// starts a batch for the fetches.
    async fn artwork_pass(&self, now: i64, network: NetworkState) -> Result<Option<Job>> {
        let snapshot = self
            .inner
            .store
            .read(move |connection| {
                let now = Timestamp::from_epoch_ms(now).context("read the pass time")?;
                artwork::load(connection, now)
            })
            .await
            .context("load the artwork tables")?;
        let retry_at = self
            .inner
            .artwork
            .retry_at
            .lock()
            .expect("no thread panics while holding the lock")
            .clone();
        // TODO: a failed URL is retried by the first pass after its retry
        // time, but nothing wakes the loop for it.
        let plan = artwork::plan(&snapshot, now, network, &retry_at);
        tracing::debug!(?plan, "artwork pass");

        let fetch = plan.fetch.clone();
        if !plan.evict.is_empty() || !plan.touch.is_empty() {
            let deleted = self
                .inner
                .store
                .write(move |transaction| artwork::apply(transaction, &plan, now))
                .await
                .context("apply the artwork plan")?;
            let root = self.inner.artwork.root.clone();
            tokio::task::spawn_blocking(move || artwork::remove_files(&root, &deleted))
                .await
                .context("wait for the artwork deletions")?;
        }
        Ok(self.start_artwork(fetch))
    }

    fn start_artwork(&self, urls: Vec<String>) -> Option<Job> {
        if urls.is_empty() {
            return None;
        }
        let cancel = CancellationToken::new();
        let batch = self.inner.artwork.clone().fetch_all(urls, cancel.clone());
        let ended = self.inner.transfer_ended.clone();
        let task = tokio::spawn(async move {
            batch.await;
            ended.notify_one();
        });
        Some(Job { cancel, task })
    }

    fn start(&self, episode_id: String, run: String) -> Job {
        let handle = self.inner.downloader.start(episode_id.clone(), run);
        let ended = self.inner.transfer_ended.clone();
        let progress_map = self.inner.progress.clone();
        let mut progress = handle.progress;
        let mut transfer = handle.task;
        let task = tokio::spawn(async move {
            let result = loop {
                tokio::select! {
                    result = &mut transfer => break result,
                    changed = progress.changed() => {
                        if changed.is_err() {
                            break (&mut transfer).await;
                        }
                        let now = *progress.borrow_and_update();
                        progress_map.send_modify(|map| {
                            map.insert(episode_id.clone(), now);
                        });
                    }
                }
            };
            progress_map.send_if_modified(|map| map.remove(&episode_id).is_some());
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::error!(error = format!("{error:#}"), "transfer failed locally")
                }
                Err(error) => tracing::error!(%error, "transfer task panicked"),
            }
            ended.notify_one();
        });
        Job {
            cancel: handle.cancel,
            task,
        }
    }

    /// Repairs what a crash left behind (spec 7.4). Runs before any
    /// transfer starts; the first pass then requeues the `transferring`
    /// rows this leaves, since none of them has a live job.
    async fn repair(&self) -> Result<()> {
        let _jobs = self.inner.jobs.lock().await;
        let root = self.inner.downloader.root.clone();
        let rows = self
            .inner
            .store
            .read(repair_rows)
            .await
            .context("read the download rows")?;

        // A run whose final file exists with the expected size crashed
        // between its rename and its row write: publish it. A complete
        // row without its file is deleted; the next pass re-queues it if
        // it is still wanted.
        let (published, missing) = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || {
                let mut published = Vec::new();
                let mut missing = Vec::new();
                for row in rows {
                    match (row.state.as_str(), &row.run_id, &row.files) {
                        ("transferring", Some(run), Some((podcast_id, audio_type))) => {
                            let Ok(files) =
                                RunFiles::new(&root, podcast_id, &row.episode_id, run, audio_type)
                            else {
                                continue;
                            };
                            let size = std::fs::metadata(&files.finished)
                                .ok()
                                .filter(|metadata| metadata.is_file())
                                .map(|metadata| metadata.len());
                            if let Some(size) = size
                                .filter(|&size| row.bytes_total.is_none_or(|total| total == size))
                            {
                                published.push((row.episode_id, run.clone(), files.relative, size));
                            }
                        }
                        ("complete", _, _) if !root.join(&row.path).is_file() => {
                            missing.push((row.episode_id, row.run_id));
                        }
                        _ => {}
                    }
                }
                (published, missing)
            })
            .await
            .context("wait for the file checks")?
        };
        let now = Timestamp::now().as_epoch_ms();
        self.inner
            .store
            .write(move |transaction| {
                for (episode_id, run, path, size) in published {
                    transaction
                        .execute(
                            "update download set state = 'complete', path = ?3,
                                 completed_at = ?4, bytes_done = ?5, wait_reason = null,
                                 last_error = null
                             where episode_id = ?1 and run_id = ?2 and state = 'transferring'",
                            params![
                                episode_id,
                                run,
                                path,
                                now,
                                i64::try_from(size).unwrap_or(i64::MAX)
                            ],
                        )
                        .context("publish a finished run")?;
                }
                for (episode_id, run) in missing {
                    transaction
                        .execute(
                            "delete from download
                             where episode_id = ?1 and run_id is ?2 and state = 'complete'",
                            params![episode_id, run],
                        )
                        .context("delete a complete row without its file")?;
                }
                Ok(())
            })
            .await
            .context("record the repairs")?;

        // Files and `.part` files no row accounts for.
        let rows = self
            .inner
            .store
            .read(repair_rows)
            .await
            .context("read the repaired rows")?;
        tokio::task::spawn_blocking(move || sweep(&root, &rows))
            .await
            .context("wait for the sweep")?;

        let files: HashSet<String> = self
            .inner
            .store
            .read(|connection| {
                connection
                    .prepare("select file from artwork")
                    .context("prepare the artwork file query")?
                    .query_map([], |row| row.get(0))
                    .context("query artwork")?
                    .collect::<rusqlite::Result<_>>()
                    .context("read artwork")
            })
            .await
            .context("read the artwork files")?;
        let root = self.inner.artwork.root.clone();
        tokio::task::spawn_blocking(move || artwork::sweep(&root, &files))
            .await
            .context("wait for the artwork sweep")?;
        Ok(())
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

/// A `download` row as the startup repair reads it.
#[derive(Debug)]
struct RepairRow {
    episode_id: String,
    state: String,
    path: String,
    bytes_total: Option<u64>,
    run_id: Option<String>,
    /// The episode's `(podcast_id, audio_type)`, which name the run's files.
    files: Option<(String, String)>,
}

fn repair_rows(connection: &Connection) -> Result<Vec<RepairRow>> {
    connection
        .prepare(
            "select d.episode_id, d.state, d.path, d.bytes_total, d.run_id,
                 e.podcast_id, e.audio_type
             from download d left join episode e on e.id = d.episode_id",
        )
        .context("prepare the repair query")?
        .query_map([], |row| {
            let podcast_id: Option<String> = row.get(5)?;
            let audio_type: Option<String> = row.get(6)?;
            Ok(RepairRow {
                episode_id: row.get(0)?,
                state: row.get(1)?,
                path: row.get(2)?,
                bytes_total: row
                    .get::<_, Option<i64>>(3)?
                    .and_then(|total| u64::try_from(total).ok()),
                run_id: row.get(4)?,
                files: podcast_id.zip(audio_type),
            })
        })
        .context("query download")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read download")
}

/// Deletes every file under `root` that no row accounts for: a `complete`
/// row accounts for its `path`, any other row with a run for its run's
/// `.part`.
fn sweep(root: &Path, rows: &[RepairRow]) {
    let accounts_for = |relative: &str, name: &str| {
        rows.iter()
            .any(|row| match (row.state.as_str(), &row.run_id) {
                ("complete", _) => row.path == relative,
                (_, Some(run)) => {
                    name.starts_with(&format!("{}.{run}.", row.episode_id))
                        && name.ends_with(".part")
                }
                (_, None) => false,
            })
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            remove_file(&path);
            continue;
        }
        let podcast = entry.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(&path) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if file.path().is_file() && !accounts_for(&format!("{podcast}/{name}"), &name) {
                tracing::info!(path = %file.path().display(), "deleting a download file no row accounts for");
                remove_file(&file.path());
            }
        }
    }
}
