//! One transfer of one episode's audio (spec 7.2, 7.3).
//!
//! The reconciler (spec 7.4) owns the `download` rows: it moves a row to
//! `transferring` under a fresh `run_id` and calls [`Downloader::start`].
//! From then on the transfer is the only writer of that row, and every write
//! it makes carries `WHERE run_id = :run_id`. If a write matches no row, the
//! row was deleted (wish removed, eviction, logout) or handed to a newer run,
//! so the transfer deletes its own files and stops without re-creating
//! anything.
//!
//! The transfer leaves the row in the state its outcome calls for
//! (`complete`, `queued`, `waiting_retry` or `failed`) and reports the same
//! [`Outcome`] through its join handle. A cancelled transfer checkpoints and
//! leaves the row `transferring`; what happens next is the canceller's call.
//!
//! Byte progress for the UI is in memory only ([`TransferHandle::progress`]).
//! The row gets a durable checkpoint at most every
//! [`Downloader::checkpoint_every`], after an fsync, so `bytes_done` never
//! exceeds what is safely on disk.

use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Report, Result, WrapErr, ensure};
use futures_util::StreamExt as _;
use reqwest::StatusCode;
use reqwest::header::{
    CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderMap, IF_RANGE, LAST_MODIFIED, RANGE,
};
use rusqlite::{OptionalExtension as _, params};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::net::NetworkState;
use crate::store::Store;

/// How often a running transfer writes `bytes_done` to its row (spec 7.3).
pub const CHECKPOINT_EVERY: Duration = Duration::from_secs(5);

/// A transient error on this attempt fails the download for good (spec 7.2).
const MAX_ATTEMPTS: u32 = 8;
const RETRY_BASE: Duration = Duration::from_secs(30);
const RETRY_CAP: Duration = Duration::from_secs(6 * 60 * 60);

// ==============================================================================
// API
// ==============================================================================

/// What every transfer shares. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Downloader {
    pub store: Store,
    pub client: reqwest::Client,
    /// The downloads root. Files live at
    /// `<root>/<podcast_id>/<episode_id>.<run_id>.<ext>[.part]`.
    pub root: PathBuf,
    /// A request error while the OS reports no connection is a "no
    /// connection" error, whatever reqwest says it was.
    pub network: watch::Receiver<NetworkState>,
    pub checkpoint_every: Duration,
}

/// In-memory progress for the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

/// Why a `queued` row isn't running, as stored in `download.wait_reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Connection,
    /// Held back on a metered network (spec 7.4).
    Wifi,
    Space,
    Budget,
}

impl WaitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            WaitReason::Connection => "connection",
            WaitReason::Wifi => "wifi",
            WaitReason::Space => "space",
            WaitReason::Budget => "budget",
        }
    }

    pub fn parse(text: &str) -> Option<WaitReason> {
        [
            WaitReason::Connection,
            WaitReason::Wifi,
            WaitReason::Space,
            WaitReason::Budget,
        ]
        .into_iter()
        .find(|reason| reason.as_str() == text)
    }
}

/// How a transfer ended. Every outcome but `Cancelled` and `LostOwnership`
/// names the state the transfer left its row in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    /// No connection or disk full. `attempts` is unchanged.
    Queued(WaitReason),
    WaitingRetry {
        attempts: u32,
    },
    Failed,
    /// The row is still `transferring` with a fresh checkpoint.
    Cancelled,
    /// The row was gone or belonged to another run. The transfer deleted
    /// its own files and wrote nothing.
    LostOwnership,
}

/// A running transfer, as the reconciler holds it.
#[derive(Debug)]
pub struct TransferHandle {
    /// Cancelling checkpoints and ends the transfer with
    /// [`Outcome::Cancelled`]. Aborting `task` instead is a crash: no
    /// checkpoint, and the next run resumes from the last one.
    pub cancel: CancellationToken,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the downloads UI is the first reader")
    )]
    pub progress: watch::Receiver<Progress>,
    /// `Err` only when the store itself fails; the row then stays
    /// `transferring` for the startup repair (spec 7.4).
    pub task: JoinHandle<Result<Outcome>>,
}

/// The client settings every download uses. Tests add `no_proxy()`.
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(format!("pod-desktop/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        // A per-read timeout rather than a total one: an hour-long episode
        // on a slow link is fine as long as bytes keep coming.
        .read_timeout(Duration::from_secs(60))
        // Byte offsets in `Range`, `Content-Range` and `bytes_done` count
        // the encoded body, so a transparently decompressed response could
        // not be resumed. The sync client shares the crate's reqwest and
        // turns `gzip` on, hence the explicit opt-out.
        .no_gzip()
}

impl Downloader {
    /// The production setup: files under `<data_dir>/audio`, where
    /// `data_dir` is `dirs::data_dir()/pod`.
    pub fn new(
        store: Store,
        network: watch::Receiver<NetworkState>,
        data_dir: &Path,
    ) -> Result<Downloader> {
        Ok(Downloader {
            store,
            client: client_builder().build().context("build the HTTP client")?,
            root: data_dir.join("audio"),
            network,
            checkpoint_every: CHECKPOINT_EVERY,
        })
    }

    /// Spawns the transfer for the row `episode_id` owned by `run_id` on
    /// the current tokio runtime.
    pub fn start(&self, episode_id: String, run_id: String) -> TransferHandle {
        let cancel = CancellationToken::new();
        let (progress, progress_rx) = watch::channel(Progress::default());
        let downloader = self.clone();
        let token = cancel.clone();
        let task = tokio::spawn(async move {
            downloader
                .run(episode_id, run_id, token, progress)
                .await
                .context("run the transfer")
        });
        TransferHandle {
            cancel,
            progress: progress_rx,
            task,
        }
    }

    async fn run(
        self,
        episode_id: String,
        run_id: String,
        cancel: CancellationToken,
        progress: watch::Sender<Progress>,
    ) -> Result<Outcome> {
        let claimed_id = (episode_id.clone(), run_id.clone());
        let claim = self
            .store
            .read(move |connection| {
                connection
                    .query_row(
                        "select d.audio_url, d.bytes_done, d.bytes_total, d.validator,
                             e.podcast_id, e.audio_type
                         from download d join episode e on e.id = d.episode_id
                         where d.episode_id = ?1 and d.run_id = ?2",
                        params![claimed_id.0, claimed_id.1],
                        |row| {
                            Ok(Claim {
                                audio_url: row.get(0)?,
                                bytes_done: row.get::<_, i64>(1)?.try_into().unwrap_or(0),
                                bytes_total: row
                                    .get::<_, Option<i64>>(2)?
                                    .and_then(|total| total.try_into().ok()),
                                validator: row.get(3)?,
                                podcast_id: row.get(4)?,
                                audio_type: row.get(5)?,
                            })
                        },
                    )
                    .optional()
                    .context("read the download row")
            })
            .await
            .context("claim the download row")?;
        // Without a row there is no path to clean up either. The startup
        // repair deletes files no row accounts for.
        let Some(claim) = claim else {
            return Ok(Outcome::LostOwnership);
        };
        let files = RunFiles::new(
            &self.root,
            &claim.podcast_id,
            &episode_id,
            &run_id,
            &claim.audio_type,
        )
        .context("name the run's files")?;
        let transfer = Transfer {
            downloader: self,
            episode_id,
            run_id,
            files,
            progress,
        };
        let result = transfer.fetch(&claim, &cancel).await;
        let outcome = transfer.finish(result).await;
        tracing::info!(
            episode_id = transfer.episode_id,
            run_id = transfer.run_id,
            ?outcome,
            "transfer ended"
        );
        outcome
    }
}

// ==============================================================================
// Files
// ==============================================================================

/// The row's fields a transfer starts from.
#[derive(Debug)]
struct Claim {
    audio_url: String,
    bytes_done: u64,
    bytes_total: Option<u64>,
    validator: Option<String>,
    podcast_id: String,
    audio_type: String,
}

/// A run's two file names. Both carry the run id, so two runs never share a
/// `.part` and a stale run can't rename over a newer run's file (spec 7.1).
#[derive(Debug)]
pub struct RunFiles {
    pub part: PathBuf,
    pub finished: PathBuf,
    /// `finished` relative to the root, as `download.path` records it.
    pub relative: String,
}

impl RunFiles {
    pub fn new(
        root: &Path,
        podcast_id: &str,
        episode_id: &str,
        run_id: &str,
        audio_type: &str,
    ) -> Result<RunFiles> {
        // Ids arrive from the server. One holding a `/` or `..` would point
        // outside the downloads root.
        for id in [podcast_id, episode_id, run_id] {
            ensure!(
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "{id:?} is not a safe file name component"
            );
        }
        let name = format!("{episode_id}.{run_id}.{}", extension(audio_type));
        let finished = root.join(podcast_id).join(&name);
        Ok(RunFiles {
            part: finished.with_extension(format!("{}.part", extension(audio_type))),
            finished,
            relative: format!("{podcast_id}/{name}"),
        })
    }
}

/// The file extension for an enclosure MIME type. Players sniff the content,
/// so an unknown type only costs a less descriptive name.
fn extension(audio_type: &str) -> &'static str {
    let essence = audio_type.split(';').next().unwrap_or_default().trim();
    match essence.to_ascii_lowercase().as_str() {
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" | "audio/aac" | "audio/x-aac" => "m4a",
        "audio/ogg" | "application/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        _ => "audio",
    }
}

// ==============================================================================
// Errors
// ==============================================================================

/// The error classes of spec 7.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorClass {
    NoConnection,
    DiskFull,
    Transient,
    Permanent,
}

/// Why `fetch` stopped short of publishing.
#[derive(Debug)]
enum Stop {
    Error(ErrorClass, String),
    Cancelled,
    LostOwnership,
    /// The store failed. Nothing more can be written.
    Internal(Report),
}

impl From<Report> for Stop {
    fn from(report: Report) -> Self {
        Stop::Internal(report)
    }
}

fn status_class(status: StatusCode) -> ErrorClass {
    match status.as_u16() {
        408 | 429 | 500..=599 => ErrorClass::Transient,
        // 404, 410 and every other 4xx, plus whatever a redirect chain
        // left unresolved.
        _ => ErrorClass::Permanent,
    }
}

fn local_io(error: io::Error) -> Stop {
    let class = match error.kind() {
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => ErrorClass::DiskFull,
        _ => ErrorClass::Transient,
    };
    Stop::Error(class, format!("local file: {error}"))
}

fn retry_delay(attempts: u32) -> Duration {
    let factor = 1u32.checked_shl(attempts).unwrap_or(u32::MAX);
    RETRY_BASE.saturating_mul(factor).min(RETRY_CAP)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis().try_into().unwrap_or(i64::MAX))
}

/// `(first byte, complete length)` from a `Content-Range` such as
/// `bytes 100-199/200` or `bytes */200`. `None` parts are `*` or absent.
fn content_range(headers: &HeaderMap) -> (Option<u64>, Option<u64>) {
    let parse = |value: &str| -> Option<(Option<u64>, Option<u64>)> {
        let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
        let start = range
            .split_once('-')
            .and_then(|(start, _)| start.parse().ok());
        Some((start, total.parse().ok()))
    };
    headers
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse)
        .unwrap_or_default()
}

// ==============================================================================
// Transfer
// ==============================================================================

#[derive(Debug)]
struct Transfer {
    downloader: Downloader,
    episode_id: String,
    run_id: String,
    files: RunFiles,
    progress: watch::Sender<Progress>,
}

impl Transfer {
    /// Runs one guarded `UPDATE` (or a transaction ending in one) that
    /// returns how many rows it changed, and turns zero into
    /// [`Stop::LostOwnership`].
    async fn write_owned<F>(&self, write: F) -> Result<(), Stop>
    where
        F: FnOnce(&rusqlite::Transaction<'_>, &str, &str) -> Result<usize> + Send + 'static,
    {
        let (episode_id, run_id) = (self.episode_id.clone(), self.run_id.clone());
        let changed = self
            .downloader
            .store
            .write(move |transaction| write(transaction, &episode_id, &run_id))
            .await
            .context("write the download row")?;
        if changed == 0 {
            return Err(Stop::LostOwnership);
        }
        Ok(())
    }

    fn network_error(&self, error: &reqwest::Error) -> Stop {
        let class = if error.is_connect() || !self.downloader.network.borrow().connected {
            ErrorClass::NoConnection
        } else if error.is_builder() || error.is_redirect() {
            ErrorClass::Permanent
        } else {
            ErrorClass::Transient
        };
        Stop::Error(class, error.to_string())
    }

    /// fsyncs, then records `done` as the durable checkpoint.
    async fn checkpoint(&self, file: &mut File, done: u64) -> Result<(), Stop> {
        file.flush().await.map_err(local_io)?;
        file.sync_data().await.map_err(local_io)?;
        let done = i64::try_from(done).unwrap_or(i64::MAX);
        self.write_owned(move |transaction, episode_id, run_id| {
            transaction
                .execute(
                    "update download set bytes_done = ?3 where episode_id = ?1 and run_id = ?2",
                    params![episode_id, run_id, done],
                )
                .context("checkpoint bytes_done")
        })
        .await
    }

    /// Checkpoints before pausing on `stop`, so the next attempt resumes
    /// from here. Losing the row overrides `stop`; a failed checkpoint
    /// otherwise leaves the older one, which is still safe.
    async fn checkpoint_then(&self, file: &mut File, done: u64, stop: Stop) -> Stop {
        match self.checkpoint(file, done).await {
            Err(lost @ (Stop::LostOwnership | Stop::Internal(_))) => lost,
            Ok(()) | Err(_) => stop,
        }
    }

    /// Drops the partial file and checkpoint after the server answered a
    /// range request with something that can't continue it, then retries
    /// as a transient error.
    async fn restart_later(&self, file: &mut File, reason: String) -> Result<(), Stop> {
        file.set_len(0).await.map_err(local_io)?;
        self.write_owned(|transaction, episode_id, run_id| {
            transaction
                .execute(
                    "update download set bytes_done = 0, validator = null
                     where episode_id = ?1 and run_id = ?2",
                    params![episode_id, run_id],
                )
                .context("reset the checkpoint")
        })
        .await?;
        Err(Stop::Error(ErrorClass::Transient, reason))
    }

    async fn fetch(&self, claim: &Claim, cancel: &CancellationToken) -> Result<(), Stop> {
        let directory = self
            .files
            .part
            .parent()
            .expect("a run's file sits in its podcast's directory");
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(local_io)?;
        let mut file = File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.files.part)
            .await
            .map_err(local_io)?;

        // Resume from the checkpoint, never from the file's length: past the
        // checkpoint the file may hold a torn tail (spec 7.3 step 1).
        let on_disk = file.metadata().await.map_err(local_io)?.len();
        let resume = claim
            .validator
            .as_deref()
            .filter(|_| claim.bytes_done > 0 && on_disk >= claim.bytes_done);
        let mut done = if resume.is_some() {
            claim.bytes_done
        } else {
            0
        };
        file.set_len(done).await.map_err(local_io)?;
        file.seek(SeekFrom::Start(done)).await.map_err(local_io)?;

        let mut request = self.downloader.client.get(&claim.audio_url);
        if let Some(validator) = resume {
            request = request
                .header(RANGE, format!("bytes={done}-"))
                .header(IF_RANGE, validator);
        }
        let response = cancel
            .run_until_cancelled(request.send())
            .await
            .ok_or(Stop::Cancelled)?
            .map_err(|error| self.network_error(&error))?;

        let status = response.status();
        let headers = response.headers();
        let html = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("text/html")
            });
        if status.is_success() && html {
            // A login wall or an error page served with 200.
            return Err(Stop::Error(
                ErrorClass::Permanent,
                "the server sent an HTML page instead of audio".to_owned(),
            ));
        }
        let total = match status {
            StatusCode::PARTIAL_CONTENT if resume.is_some() => match content_range(headers) {
                (Some(start), total) if start == done => total,
                _ => {
                    return self
                        .restart_later(&mut file, "206 for a different range".to_owned())
                        .await;
                }
            },
            StatusCode::OK => {
                // No range, or `If-Range` saw a changed file.
                done = 0;
                file.set_len(0).await.map_err(local_io)?;
                file.seek(SeekFrom::Start(0)).await.map_err(local_io)?;
                response.content_length()
            }
            StatusCode::RANGE_NOT_SATISFIABLE if resume.is_some() => {
                let total = content_range(headers).1.or(claim.bytes_total);
                if total != Some(done) {
                    return self
                        .restart_later(&mut file, "416 for an incomplete file".to_owned())
                        .await;
                }
                // The checkpoint already covers the whole file.
                self.record_response(total, claim.validator.clone(), done)
                    .await?;
                return self.publish(file, done).await;
            }
            _ => return Err(Stop::Error(status_class(status), format!("HTTP {status}"))),
        };
        let validator = headers
            .get(ETAG)
            .or_else(|| headers.get(LAST_MODIFIED))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        self.record_response(total, validator, done).await?;

        let mut body = response.bytes_stream();
        let mut last_checkpoint = Instant::now();
        loop {
            self.progress.send_replace(Progress {
                bytes_done: done,
                bytes_total: total,
            });
            let Some(next) = cancel.run_until_cancelled(body.next()).await else {
                self.checkpoint(&mut file, done).await?;
                return Err(Stop::Cancelled);
            };
            let chunk = match next {
                None => break,
                Some(Ok(chunk)) => chunk,
                Some(Err(error)) => {
                    let stop = self.network_error(&error);
                    return Err(self.checkpoint_then(&mut file, done, stop).await);
                }
            };
            if let Err(error) = file.write_all(&chunk).await {
                return Err(self.checkpoint_then(&mut file, done, local_io(error)).await);
            }
            done += chunk.len() as u64;
            if last_checkpoint.elapsed() >= self.downloader.checkpoint_every {
                self.checkpoint(&mut file, done).await?;
                last_checkpoint = Instant::now();
            }
        }

        if let Some(total) = total.filter(|&total| total != done) {
            let stop = Stop::Error(
                ErrorClass::Transient,
                format!("the body ended after {done} of {total} bytes"),
            );
            return Err(self.checkpoint_then(&mut file, done, stop).await);
        }
        self.publish(file, done).await
    }

    /// Records what the response says about the file: its size on the row
    /// and in `known_size`, the validator for `If-Range`, and where this
    /// response starts writing.
    async fn record_response(
        &self,
        total: Option<u64>,
        validator: Option<String>,
        done: u64,
    ) -> Result<(), Stop> {
        let total = total.and_then(|total| i64::try_from(total).ok());
        let done = i64::try_from(done).unwrap_or(i64::MAX);
        self.write_owned(move |transaction, episode_id, run_id| {
            let changed = transaction
                .execute(
                    "update download set bytes_total = ?3, validator = ?4, bytes_done = ?5
                     where episode_id = ?1 and run_id = ?2",
                    params![episode_id, run_id, total, validator, done],
                )
                .context("record the response headers")?;
            if let Some(total) = total.filter(|_| changed > 0) {
                transaction
                    .execute(
                        "insert into known_size (episode_id, bytes) values (?1, ?2)
                         on conflict (episode_id) do update set bytes = excluded.bytes",
                        params![episode_id, total],
                    )
                    .context("record the known size")?;
            }
            Ok(changed)
        })
        .await
    }

    /// fsync, rename to the final name, then mark the row complete in one
    /// transaction (spec 7.2). Readers find files through `download.path`,
    /// so the file only counts once that commits.
    async fn publish(&self, mut file: File, done: u64) -> Result<(), Stop> {
        file.flush().await.map_err(local_io)?;
        file.sync_data().await.map_err(local_io)?;
        drop(file);
        tokio::fs::rename(&self.files.part, &self.files.finished)
            .await
            .map_err(local_io)?;
        let (path, done) = (
            self.files.relative.clone(),
            i64::try_from(done).unwrap_or(i64::MAX),
        );
        self.write_owned(move |transaction, episode_id, run_id| {
            transaction
                .execute(
                    "update download set state = 'complete', path = ?3, completed_at = ?4,
                         bytes_done = ?5, wait_reason = null, last_error = null
                     where episode_id = ?1 and run_id = ?2",
                    params![episode_id, run_id, path, now_ms(), done],
                )
                .context("mark the download complete")
        })
        .await
    }

    /// Moves the row out of `transferring` according to how `fetch` ended.
    async fn finish(&self, result: Result<(), Stop>) -> Result<Outcome> {
        let (class, message) = match result {
            Ok(()) => return Ok(Outcome::Complete),
            Err(Stop::Cancelled) => return Ok(Outcome::Cancelled),
            Err(Stop::LostOwnership) => {
                self.delete_files().await;
                return Ok(Outcome::LostOwnership);
            }
            Err(Stop::Internal(report)) => return Err(report),
            Err(Stop::Error(class, message)) => (class, message),
        };
        tracing::warn!(
            episode_id = self.episode_id,
            ?class,
            error = message,
            "transfer stopped"
        );

        let (episode_id, run_id) = (self.episode_id.clone(), self.run_id.clone());
        let recorded = self
            .downloader
            .store
            .write(move |transaction| {
                record_error(transaction, &episode_id, &run_id, class, &message)
            })
            .await
            .context("record the transfer error")?;
        match recorded {
            None => {
                self.delete_files().await;
                Ok(Outcome::LostOwnership)
            }
            Some((outcome, keep_part)) => {
                if !keep_part {
                    delete_if_present(&self.files.part).await;
                }
                Ok(outcome)
            }
        }
    }

    async fn delete_files(&self) {
        delete_if_present(&self.files.part).await;
        delete_if_present(&self.files.finished).await;
    }
}

/// Writes the state an error class leads to (spec 7.2) and returns the
/// outcome and whether the `.part` file is worth keeping. `None` if the row
/// isn't this run's.
fn record_error(
    transaction: &rusqlite::Transaction<'_>,
    episode_id: &str,
    run_id: &str,
    class: ErrorClass,
    message: &str,
) -> Result<Option<(Outcome, bool)>> {
    let row = transaction
        .query_row(
            "select attempts, bytes_done > 0 and validator is not null from download
             where episode_id = ?1 and run_id = ?2",
            params![episode_id, run_id],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()
        .context("read the attempts")?;
    let Some((attempts, resumable)) = row else {
        return Ok(None);
    };
    let (outcome, keep_part) = match class {
        ErrorClass::NoConnection => (Outcome::Queued(WaitReason::Connection), true),
        ErrorClass::DiskFull => (Outcome::Queued(WaitReason::Space), true),
        ErrorClass::Transient if attempts + 1 < MAX_ATTEMPTS => (
            Outcome::WaitingRetry {
                attempts: attempts + 1,
            },
            true,
        ),
        // Keep the `.part` only if a user retry could resume it.
        ErrorClass::Transient | ErrorClass::Permanent => (Outcome::Failed, resumable),
    };
    let attempts = match class {
        ErrorClass::Transient => attempts + 1,
        _ => attempts,
    };
    let state = match outcome {
        Outcome::Queued(_) => "queued",
        Outcome::WaitingRetry { .. } => "waiting_retry",
        _ => "failed",
    };
    let wait_reason = match outcome {
        Outcome::Queued(reason) => Some(reason.as_str()),
        _ => None,
    };
    let next_attempt_at = match outcome {
        Outcome::WaitingRetry { attempts } => {
            let delay = i64::try_from(retry_delay(attempts).as_millis()).unwrap_or(i64::MAX);
            Some(now_ms().saturating_add(delay))
        }
        _ => None,
    };
    transaction
        .execute(
            "update download set state = ?3, attempts = ?4, next_attempt_at = ?5,
                 wait_reason = ?6, last_error = ?7,
                 bytes_done = case when ?8 then bytes_done else 0 end,
                 validator = case when ?8 then validator else null end
             where episode_id = ?1 and run_id = ?2",
            params![
                episode_id,
                run_id,
                state,
                attempts,
                next_attempt_at,
                wait_reason,
                message,
                keep_part
            ],
        )
        .context("move the row out of transferring")?;
    Ok(Some((outcome, keep_part)))
}

async fn delete_if_present(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        // The startup repair deletes files no row accounts for.
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "can't delete a download file")
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
