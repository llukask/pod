//! Transfers against a live local HTTP server.
//!
//! Every test that needs a socket lives in this module and shares one server
//! on a port from [`bind_test_port`], because sandboxed development
//! environments can only bind a few fixed ports (see CLAUDE.md).
//! Each test uses its own ETag in the URL, so the per-ETag request log stays
//! separate while the tests run concurrently.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rusqlite::params;
use tempfile::TempDir;

use super::*;
use crate::store::tests::scratch_dir;
use crate::test_port::bind_test_port;

// ==============================================================================
// Shared server
// ==============================================================================

pub(crate) const BODY_LEN: usize = 64 * 1024;

/// The audio every `/audio/{etag}` URL serves: a pattern whose period (251)
/// doesn't divide any chunk size, so a duplicated or skipped range shows up
/// as a content mismatch.
fn body() -> &'static [u8] {
    static BODY: OnceLock<Vec<u8>> = OnceLock::new();
    BODY.get_or_init(|| (0..BODY_LEN).map(|index| (index % 251) as u8).collect())
}

/// The `Range` header of every request per ETag, in arrival order.
fn ranges() -> &'static Mutex<HashMap<String, Vec<Option<String>>>> {
    static RANGES: OnceLock<Mutex<HashMap<String, Vec<Option<String>>>>> = OnceLock::new();
    RANGES.get_or_init(Mutex::default)
}

pub(crate) fn ranges_seen(etag: &str) -> Vec<Option<String>> {
    ranges()
        .lock()
        .expect("no test panics while holding the lock")
        .get(etag)
        .cloned()
        .unwrap_or_default()
}

/// The shared server's base URL. The first call binds a port with
/// [`bind_test_port`] (`POD_TEST_DOWNLOAD_PORT` overrides the choice) and
/// starts the server on its own thread and runtime, so it outlives each
/// test's runtime.
pub(crate) fn ensure_server() -> String {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        let listener = bind_test_port("POD_TEST_DOWNLOAD_PORT");
        let address = listener
            .local_addr()
            .expect("a bound listener has an address");
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime builds");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("the listener joins the runtime");
                axum::serve(listener, routes())
                    .await
                    .expect("the test server runs until the process exits");
            });
        });
        format!("http://{address}")
    })
    .clone()
}

fn routes() -> Router {
    Router::new()
        .route("/audio/{etag}", get(audio))
        .route("/status/{code}", get(status))
        .route("/html", get(html))
        .route("/image/{name}", get(image))
        .route("/cover/{name}", get(cover))
}

/// A 1x1 PNG, the body of every `/image/{name}`.
pub(crate) const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xdf, 0xc0, 0xf0,
    0x1f, 0x00, 0x06, 0x80, 0x02, 0x7f, 0x10, 0x4c, 0x1b, 0xe1, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// How many times each `/image/{name}` was requested.
fn image_hits() -> &'static Mutex<HashMap<String, usize>> {
    static HITS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    HITS.get_or_init(Mutex::default)
}

pub(crate) fn image_requests(name: &str) -> usize {
    image_hits()
        .lock()
        .expect("no test panics while holding the lock")
        .get(name)
        .copied()
        .unwrap_or(0)
}

async fn image(UrlPath(name): UrlPath<String>) -> Response {
    *image_hits()
        .lock()
        .expect("no test panics while holding the lock")
        .entry(name)
        .or_default() += 1;
    ([(header::CONTENT_TYPE, "image/png")], PNG).into_response()
}

/// A 1500x1500 JPEG gradient, the body of every `/cover/{name}`: a cover
/// larger than the artwork cache keeps.
pub(crate) fn cover_body() -> &'static [u8] {
    static COVER: OnceLock<Vec<u8>> = OnceLock::new();
    COVER.get_or_init(|| {
        let cover = image::RgbImage::from_fn(1500, 1500, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut body = std::io::Cursor::new(Vec::new());
        cover
            .write_to(&mut body, image::ImageFormat::Jpeg)
            .expect("the cover encodes");
        body.into_inner()
    })
}

async fn cover(UrlPath(name): UrlPath<String>) -> Response {
    *image_hits()
        .lock()
        .expect("no test panics while holding the lock")
        .entry(name)
        .or_default() += 1;
    ([(header::CONTENT_TYPE, "image/jpeg")], cover_body()).into_response()
}

/// Serves [`body`] with `ETag: "<etag>"`, honouring `Range: bytes=<start>-`
/// and `If-Range` the way a static file server does. `?trickle` sends 1 KiB
/// every 5 ms, so a test can act while the body is in flight.
async fn audio(
    UrlPath(etag): UrlPath<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    ranges()
        .lock()
        .expect("no test panics while holding the lock")
        .entry(etag.clone())
        .or_default()
        .push(range.clone());

    let quoted = format!("\"{etag}\"");
    let if_range_matches = headers
        .get(header::IF_RANGE)
        .is_none_or(|value| value.as_bytes() == quoted.as_bytes());
    let start = range.filter(|_| if_range_matches).and_then(|range| {
        range
            .strip_prefix("bytes=")?
            .strip_suffix('-')?
            .parse()
            .ok()
    });

    let mut response = match start {
        Some(start) if start >= BODY_LEN => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{BODY_LEN}"))],
            )
                .into_response();
        }
        Some(start) => {
            let mut response = served(&body()[start..], query.contains_key("trickle"));
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes {start}-{}/{BODY_LEN}", BODY_LEN - 1)
                    .parse()
                    .expect("a Content-Range value is a valid header"),
            );
            response
        }
        None => served(body(), query.contains_key("trickle")),
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ETAG,
        quoted.parse().expect("an ETag is a valid header"),
    );
    headers.insert(
        header::CONTENT_TYPE,
        "audio/mpeg".parse().expect("a MIME type is a valid header"),
    );
    response
}

fn served(bytes: &'static [u8], trickle: bool) -> Response {
    let mut response = if trickle {
        let chunks = futures_util::stream::iter(bytes.chunks(1024)).then(|chunk| async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok::<_, Infallible>(Bytes::from_static(chunk))
        });
        Body::from_stream(chunks).into_response()
    } else {
        Body::from(bytes).into_response()
    };
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, bytes.len().into());
    response
}

async fn status(UrlPath(code): UrlPath<u16>) -> StatusCode {
    StatusCode::from_u16(code).expect("tests ask for valid status codes")
}

async fn html() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<html>a login wall</html>",
    )
}

// ==============================================================================
// Fixture
// ==============================================================================

const PODCAST: &str = "11111111-1111-5111-8111-111111111111";
const RUN: &str = "0199a000-0000-7000-8000-000000000001";

struct Fixture {
    _dir: TempDir,
    store: Store,
    downloader: Downloader,
    root: PathBuf,
    /// Held so the downloader's receiver never sees the channel closed.
    _network: watch::Sender<NetworkState>,
}

fn fixture(checkpoint_every: Duration) -> Fixture {
    let dir = scratch_dir();
    let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
    let root = dir.path().join("audio");
    let (network, network_rx) = watch::channel(NetworkState::ASSUMED);
    // The sandbox routes outbound HTTP through a proxy; loopback must not.
    let client = client_builder()
        .no_proxy()
        .build()
        .expect("the test client builds");
    let downloader = Downloader {
        store: store.clone(),
        client,
        root: root.clone(),
        network: network_rx,
        checkpoint_every,
    };
    Fixture {
        _dir: dir,
        store,
        downloader,
        root,
        _network: network,
    }
}

/// The row as the reconciler leaves it when it starts a run.
#[derive(Debug)]
struct Seed<'a> {
    episode_id: &'a str,
    url: String,
    bytes_done: i64,
    validator: Option<&'a str>,
    attempts: u32,
}

impl Fixture {
    async fn seed(&self, seed: Seed<'_>) {
        let Seed {
            episode_id,
            url,
            bytes_done,
            validator,
            attempts,
        } = seed;
        let (episode_id, validator) = (episode_id.to_owned(), validator.map(str::to_owned));
        self.store
            .write(move |transaction| {
                transaction
                    .execute(
                        "insert into episode (id, podcast_id, title, summary_html, content_html,
                             published_at, audio_url, audio_type, version)
                         values (?1, ?2, 'An episode', '', '', 0, ?3, 'audio/mpeg', 1)",
                        params![episode_id, PODCAST, url],
                    )
                    .context("insert the episode")?;
                transaction
                    .execute(
                        "insert into download (episode_id, state, audio_url, path, bytes_done,
                             validator, attempts, run_id)
                         values (?1, 'transferring', ?2, '', ?3, ?4, ?5, ?6)",
                        params![episode_id, url, bytes_done, validator, attempts, RUN],
                    )
                    .context("insert the download")?;
                Ok(())
            })
            .await
            .expect("the seed commits");
    }

    async fn row(&self, episode_id: &str) -> Option<Row> {
        let episode_id = episode_id.to_owned();
        self.store
            .read(move |connection| {
                connection
                    .query_row(
                        "select d.state, d.path, d.bytes_done, d.bytes_total, d.validator,
                             d.attempts, d.next_attempt_at, d.wait_reason, k.bytes
                         from download d left join known_size k using (episode_id)
                         where d.episode_id = ?1",
                        params![episode_id],
                        |row| {
                            Ok(Row {
                                state: row.get(0)?,
                                path: row.get(1)?,
                                bytes_done: row.get(2)?,
                                bytes_total: row.get(3)?,
                                validator: row.get(4)?,
                                attempts: row.get(5)?,
                                next_attempt_at: row.get(6)?,
                                wait_reason: row.get(7)?,
                                known_size: row.get(8)?,
                            })
                        },
                    )
                    .optional()
                    .context("read the download row")
            })
            .await
            .expect("the read runs")
    }

    /// Polls the row until its durable checkpoint reaches `bytes`.
    async fn wait_for_checkpoint(&self, episode_id: &str, bytes: i64) -> i64 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(done) = self
                .row(episode_id)
                .await
                .map(|row| row.bytes_done)
                .filter(|&done| done >= bytes)
            {
                return done;
            }
            assert!(Instant::now() < deadline, "no checkpoint of {bytes} bytes");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn part(&self, episode_id: &str) -> PathBuf {
        self.root
            .join(PODCAST)
            .join(format!("{episode_id}.{RUN}.mp3.part"))
    }

    fn finished(&self, episode_id: &str) -> PathBuf {
        self.root
            .join(PODCAST)
            .join(format!("{episode_id}.{RUN}.mp3"))
    }

    fn start(&self, episode_id: &str) -> TransferHandle {
        self.downloader.start(episode_id.to_owned(), RUN.to_owned())
    }
}

#[derive(Debug)]
struct Row {
    state: String,
    path: String,
    bytes_done: i64,
    bytes_total: Option<i64>,
    validator: Option<String>,
    attempts: u32,
    next_attempt_at: Option<i64>,
    wait_reason: Option<String>,
    known_size: Option<i64>,
}

async fn outcome(handle: TransferHandle) -> Outcome {
    tokio::time::timeout(Duration::from_secs(20), handle.task)
        .await
        .expect("the transfer ends within 20 s")
        .expect("the transfer task doesn't panic")
        .expect("the transfer reaches an outcome")
}

fn write_part(path: &std::path::Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a part file has a directory"))
        .expect("the podcast directory is creatable");
    std::fs::write(path, bytes).expect("the part file is writable");
}

// ==============================================================================
// Acceptance
// ==============================================================================

#[tokio::test]
async fn a_killed_transfer_resumes_without_duplicate_bytes() {
    let server = ensure_server();
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000001";
    let url = format!("{server}/audio/kill?trickle");
    fixture
        .seed(Seed {
            episode_id: episode,
            url,
            bytes_done: 0,
            validator: None,
            attempts: 0,
        })
        .await;

    let first = fixture.start(episode);
    let checkpoint = fixture.wait_for_checkpoint(episode, 8 * 1024).await;
    first.task.abort();
    let _ = first.task.await;
    // Aborting drops the transfer, but a checkpoint it already handed to the
    // writer thread still commits; a killed process would write nothing more.
    // The writer runs jobs in order, so an empty write waits that one out.
    fixture
        .store
        .write(|_| Ok(()))
        .await
        .expect("the empty write commits");
    // The process died after the checkpoint: the file may hold more than the
    // checkpoint, and the unsynced tail may be garbage.
    let checkpoint = fixture
        .row(episode)
        .await
        .map_or(checkpoint, |row| row.bytes_done);
    let mut torn = std::fs::read(fixture.part(episode)).expect("the part file exists");
    torn.extend_from_slice(&[0xAA; 3000]);
    std::fs::write(fixture.part(episode), &torn).expect("the part file is writable");

    assert_eq!(outcome(fixture.start(episode)).await, Outcome::Complete);

    let file = std::fs::read(fixture.finished(episode)).expect("the published file exists");
    assert!(
        file == body(),
        "the published file is the body, byte for byte"
    );
    assert!(!fixture.part(episode).exists());
    let resumed = ranges_seen("kill").pop().flatten();
    assert_eq!(resumed, Some(format!("bytes={checkpoint}-")));
    let row = fixture.row(episode).await.expect("the row survives");
    assert_eq!(row.state, "complete");
    assert_eq!(row.path, format!("{PODCAST}/{episode}.{RUN}.mp3"));
    assert_eq!(row.bytes_total, Some(BODY_LEN as i64));
    assert_eq!(row.known_size, Some(BODY_LEN as i64));
    assert_eq!(row.validator.as_deref(), Some("\"kill\""));
}

#[tokio::test]
async fn a_changed_etag_restarts_from_zero() {
    let server = ensure_server();
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000002";
    write_part(&fixture.part(episode), &[0xFF; 1000]);
    fixture
        .seed(Seed {
            episode_id: episode,
            url: format!("{server}/audio/changed"),
            bytes_done: 1000,
            validator: Some("\"before\""),
            attempts: 0,
        })
        .await;

    assert_eq!(outcome(fixture.start(episode)).await, Outcome::Complete);

    assert_eq!(ranges_seen("changed"), [Some("bytes=1000-".to_owned())]);
    let file = std::fs::read(fixture.finished(episode)).expect("the published file exists");
    assert!(file == body(), "the old prefix is gone");
    let row = fixture.row(episode).await.expect("the row survives");
    assert_eq!(row.validator.as_deref(), Some("\"changed\""));
}

#[tokio::test]
async fn a_deleted_row_makes_the_transfer_delete_its_part() {
    let server = ensure_server();
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000003";
    fixture
        .seed(Seed {
            episode_id: episode,
            url: format!("{server}/audio/deleted?trickle"),
            bytes_done: 0,
            validator: None,
            attempts: 0,
        })
        .await;

    let handle = fixture.start(episode);
    fixture.wait_for_checkpoint(episode, 1).await;
    let deleted = episode.to_owned();
    fixture
        .store
        .write(move |transaction| {
            transaction
                .execute("delete from download where episode_id = ?1", [deleted])
                .context("delete the row")
        })
        .await
        .expect("the delete commits");

    assert_eq!(outcome(handle).await, Outcome::LostOwnership);
    assert!(!fixture.part(episode).exists());
    assert!(!fixture.finished(episode).exists());
    assert!(
        fixture.row(episode).await.is_none(),
        "no write re-created it"
    );
}

#[tokio::test]
async fn a_404_ends_in_failed() {
    let server = ensure_server();
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000004";
    fixture
        .seed(Seed {
            episode_id: episode,
            url: format!("{server}/status/404"),
            bytes_done: 0,
            validator: None,
            attempts: 0,
        })
        .await;

    assert_eq!(outcome(fixture.start(episode)).await, Outcome::Failed);
    let row = fixture.row(episode).await.expect("the row survives");
    assert_eq!(row.state, "failed");
    assert!(!fixture.part(episode).exists(), "nothing to resume from");
}

// ==============================================================================
// Error classes and edge cases
// ==============================================================================

#[tokio::test]
async fn an_html_response_is_permanent() {
    let server = ensure_server();
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000005";
    fixture
        .seed(Seed {
            episode_id: episode,
            url: format!("{server}/html"),
            bytes_done: 0,
            validator: None,
            attempts: 0,
        })
        .await;

    assert_eq!(outcome(fixture.start(episode)).await, Outcome::Failed);
    assert_eq!(
        fixture.row(episode).await.map(|row| row.state).as_deref(),
        Some("failed")
    );
}

#[tokio::test]
async fn a_server_error_waits_for_a_retry_with_backoff() {
    let server = ensure_server();
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000006";
    fixture
        .seed(Seed {
            episode_id: episode,
            url: format!("{server}/status/503"),
            bytes_done: 0,
            validator: None,
            attempts: 2,
        })
        .await;

    let before = now_ms();
    assert_eq!(
        outcome(fixture.start(episode)).await,
        Outcome::WaitingRetry { attempts: 3 }
    );
    let row = fixture.row(episode).await.expect("the row survives");
    assert_eq!((row.state.as_str(), row.attempts), ("waiting_retry", 3));
    // 30 s * 2^3.
    let delay = row.next_attempt_at.expect("a retry time is set") - before;
    assert!((240_000..250_000).contains(&delay), "delay was {delay} ms");
}

#[tokio::test]
async fn the_eighth_attempt_fails_for_good() {
    let server = ensure_server();
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000007";
    fixture
        .seed(Seed {
            episode_id: episode,
            url: format!("{server}/status/500"),
            bytes_done: 0,
            validator: None,
            attempts: 7,
        })
        .await;

    assert_eq!(outcome(fixture.start(episode)).await, Outcome::Failed);
    let row = fixture.row(episode).await.expect("the row survives");
    assert_eq!((row.state.as_str(), row.attempts), ("failed", 8));
}

#[tokio::test]
async fn a_connect_error_requeues_without_using_a_retry() {
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000008";
    fixture
        .seed(Seed {
            episode_id: episode,
            // Nothing listens on port 1.
            url: "http://127.0.0.1:1/episode.mp3".to_owned(),
            bytes_done: 0,
            validator: None,
            attempts: 3,
        })
        .await;

    assert_eq!(
        outcome(fixture.start(episode)).await,
        Outcome::Queued(WaitReason::Connection)
    );
    let row = fixture.row(episode).await.expect("the row survives");
    assert_eq!(row.state, "queued");
    assert_eq!(row.wait_reason.as_deref(), Some("connection"));
    assert_eq!(row.attempts, 3);
}

#[tokio::test]
async fn a_416_with_a_full_part_counts_as_done() {
    let server = ensure_server();
    let fixture = fixture(Duration::ZERO);
    let episode = "e0000000-0000-5000-8000-000000000009";
    write_part(&fixture.part(episode), body());
    fixture
        .seed(Seed {
            episode_id: episode,
            url: format!("{server}/audio/full"),
            bytes_done: BODY_LEN as i64,
            validator: Some("\"full\""),
            attempts: 0,
        })
        .await;

    assert_eq!(outcome(fixture.start(episode)).await, Outcome::Complete);
    assert_eq!(ranges_seen("full"), [Some(format!("bytes={BODY_LEN}-"))]);
    let file = std::fs::read(fixture.finished(episode)).expect("the published file exists");
    assert!(file == body());
    let row = fixture.row(episode).await.expect("the row survives");
    assert_eq!(row.known_size, Some(BODY_LEN as i64));
}

#[tokio::test]
async fn a_cancelled_transfer_checkpoints_first() {
    let server = ensure_server();
    // No timed checkpoint fires during the test.
    let fixture = fixture(Duration::from_secs(3600));
    let episode = "e0000000-0000-5000-8000-000000000010";
    fixture
        .seed(Seed {
            episode_id: episode,
            url: format!("{server}/audio/cancel?trickle"),
            bytes_done: 0,
            validator: None,
            attempts: 0,
        })
        .await;

    let mut handle = fixture.start(episode);
    handle
        .progress
        .wait_for(|progress| progress.bytes_done >= 4096)
        .await
        .expect("the transfer reports progress");
    handle.cancel.cancel();
    let progress = *handle.progress.borrow();
    assert_eq!(outcome(handle).await, Outcome::Cancelled);

    let row = fixture
        .row(episode)
        .await
        .expect("cancelling keeps the row");
    assert!(row.bytes_done as u64 >= progress.bytes_done);
    let on_disk = std::fs::metadata(fixture.part(episode))
        .expect("the part file stays for the reconciler")
        .len();
    assert_eq!(on_disk, row.bytes_done as u64);
}

#[test]
fn retries_back_off_up_to_six_hours() {
    assert_eq!(retry_delay(1), Duration::from_secs(60));
    assert_eq!(retry_delay(7), Duration::from_secs(30 * 128));
    assert_eq!(retry_delay(40), RETRY_CAP);
}

#[test]
fn file_names_stay_inside_the_root() {
    let root = PathBuf::from("/audio");
    let files =
        RunFiles::new(&root, PODCAST, "e1", RUN, "audio/mp4; codecs=mp4a").expect("ids are safe");
    assert_eq!(
        files.finished,
        root.join(PODCAST).join(format!("e1.{RUN}.m4a"))
    );
    assert_eq!(
        files.part,
        root.join(PODCAST).join(format!("e1.{RUN}.m4a.part"))
    );
    let _ = RunFiles::new(&root, "..", "e1", RUN, "audio/mpeg").expect_err("`..` is refused");
    let _ = RunFiles::new(&root, PODCAST, "a/b", RUN, "audio/mpeg").expect_err("`/` is refused");
}
