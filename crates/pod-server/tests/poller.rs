//! The feed poller against Postgres and a live local feed server (spec 3.1,
//! 3.2 and 3.4).
//!
//! Every test that needs a socket lives in this one binary and shares one
//! server on `127.0.0.1:${POD_TEST_POLLER_PORT:-3008}` (see CLAUDE.md for the
//! sandbox's port range; `tests/fetch_guard.rs` uses 3007). Each test serves
//! its feeds under its own `/s/{name}` paths and scripts the replies there,
//! so the tests run concurrently against the one server.
//!
//! The tests drive [`claim_due`] and [`Poller::poll_once`] directly, one poll
//! at a time; only `workers_poll_due_feeds_until_shutdown` runs the worker
//! loop.

mod common;

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pod_protocol::{PodcastId, PodcastStatus, normalize_feed_url};
use pod_server::fetch::{Allowlist, FetchPolicy, Fetcher};
use pod_server::poller::{ClaimedFeed, PollOutcome, Poller, claim_due};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

// ==============================================================================
// Scripted feed server
// ==============================================================================

fn port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| {
        std::env::var("POD_TEST_POLLER_PORT")
            .ok()
            .filter(|port| !port.is_empty())
            .map(|port| port.parse().expect("POD_TEST_POLLER_PORT is a port number"))
            .unwrap_or(3008)
    })
}

/// One scripted reply.
#[derive(Debug, Clone)]
struct Reply {
    status: StatusCode,
    headers: Vec<(header::HeaderName, String)>,
    body: String,
}

impl Reply {
    fn status(code: u16) -> Reply {
        Reply {
            status: StatusCode::from_u16(code).expect("tests script valid statuses"),
            headers: Vec::new(),
            body: String::new(),
        }
    }

    fn feed(body: impl Into<String>) -> Reply {
        Reply {
            body: body.into(),
            ..Reply::status(200)
        }
    }

    fn header(mut self, name: header::HeaderName, value: impl Into<String>) -> Reply {
        self.headers.push((name, value.into()));
        self
    }
}

/// What the server knows about one `/s/{name}` path.
#[derive(Debug, Default)]
struct Route {
    /// Replies still to send, in order. The last one repeats.
    replies: VecDeque<Reply>,
    /// The headers of every request received, in order.
    requests: Vec<HeaderMap>,
}

fn routes_state() -> &'static Mutex<HashMap<String, Route>> {
    static ROUTES: OnceLock<Mutex<HashMap<String, Route>>> = OnceLock::new();
    ROUTES.get_or_init(Mutex::default)
}

/// Sets the replies `/s/{name}` sends from now on.
fn script(name: &str, replies: Vec<Reply>) {
    ensure_server();
    routes_state()
        .lock()
        .expect("no test panics while holding the lock")
        .entry(name.to_owned())
        .or_default()
        .replies = replies.into();
}

/// The headers of every request `/s/{name}` received.
fn requests(name: &str) -> Vec<HeaderMap> {
    routes_state()
        .lock()
        .expect("no test panics while holding the lock")
        .get(name)
        .map(|route| route.requests.clone())
        .unwrap_or_default()
}

async fn scripted(Path(name): Path<String>, headers: HeaderMap) -> Response {
    let mut routes = routes_state()
        .lock()
        .expect("no test panics while holding the lock");
    let route = routes.entry(name).or_default();
    route.requests.push(headers);
    let reply = if route.replies.len() > 1 {
        route.replies.pop_front()
    } else {
        route.replies.front().cloned()
    };
    let Some(reply) = reply else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = (reply.status, reply.body).into_response();
    for (name, value) in reply.headers {
        response.headers_mut().insert(
            name,
            value.parse().expect("tests script valid header values"),
        );
    }
    response
}

/// Starts the server once, on its own thread and runtime so it outlives each
/// test's runtime.
fn ensure_server() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("the test runtime builds");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port()))
                    .await
                    .expect("the test HTTP port is free (set POD_TEST_POLLER_PORT)");
                ready_tx
                    .send(())
                    .expect("the test thread waits for readiness");
                let routes = Router::new().route("/s/{name}", get(scripted));
                axum::serve(listener, routes)
                    .await
                    .expect("the test server runs until the process exits");
            });
        });
        ready_rx
            .recv()
            .expect("the server thread reports readiness");
    });
}

fn feed_url(name: &str) -> String {
    format!("http://127.0.0.1:{}/s/{name}", port())
}

/// A real feed with five episodes.
fn daily_feed() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/feeds/art19-the-daily.xml"
    ))
    .expect("the fixture exists")
}

/// [`daily_feed`] announcing that it moved to `new_url`.
fn daily_feed_moving_to(new_url: &str) -> String {
    daily_feed().replacen(
        "<channel>",
        &format!("<channel><itunes:new-feed-url>{new_url}</itunes:new-feed-url>"),
        1,
    )
}

// ==============================================================================
// Poller setup and database reads
// ==============================================================================

fn poller(pool: &PgPool) -> Poller {
    ensure_server();
    let mut policy = FetchPolicy::new(Allowlist::parse(&["127.0.0.1"]).expect("a valid allowlist"));
    policy.allowed_ports = vec![port()];
    let fetcher = Fetcher::new(policy, "https://pod.example.org").expect("the HTTP client builds");
    Poller::new(pool.clone(), fetcher, POLL_INTERVAL)
}

/// Claims the one due feed, which must be `podcast_id`'s.
async fn claim(pool: &PgPool, podcast_id: PodcastId) -> ClaimedFeed {
    let claimed = claim_due(pool)
        .await
        .expect("the claim query runs")
        .expect("a feed is due");
    assert_eq!(claimed.podcast_id, podcast_id);
    claimed
}

/// Claims and polls `podcast_id`, after making it due again.
async fn poll(poller: &Poller, pool: &PgPool, podcast_id: PodcastId) -> PollOutcome {
    make_due(pool, podcast_id).await;
    let claimed = claim(pool, podcast_id).await;
    poller.poll_once(claimed).await.expect("the poll runs")
}

async fn make_due(pool: &PgPool, podcast_id: PodcastId) {
    sqlx::query!(
        "update feed_poll set next_poll_at = now() where podcast_id = $1",
        podcast_id.as_uuid()
    )
    .execute(pool)
    .await
    .expect("the poll row exists");
}

#[derive(Debug)]
struct PollRow {
    etag: Option<String>,
    last_modified: Option<String>,
    consecutive_errors: i32,
    /// Seconds from now until `next_poll_at`, `None` for infinity.
    next_poll_in: Option<f64>,
    polled: bool,
    erroring: bool,
}

async fn poll_row(pool: &PgPool, podcast_id: PodcastId) -> PollRow {
    sqlx::query_as!(
        PollRow,
        r#"select etag, last_modified, consecutive_errors,
                  case when next_poll_at = 'infinity' then null
                       else extract(epoch from next_poll_at - now())::float8
                  end as next_poll_in,
                  last_polled_at is not null as "polled!",
                  first_error_at is not null as "erroring!"
           from feed_poll where podcast_id = $1"#,
        podcast_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("the poll row exists")
}

#[derive(Debug, PartialEq, Eq)]
struct PodcastRow {
    feed_url: String,
    status: String,
    status_message: Option<String>,
    version: i64,
}

async fn podcast_row(pool: &PgPool, podcast_id: PodcastId) -> PodcastRow {
    sqlx::query_as!(
        PodcastRow,
        "select feed_url, status, status_message, version from podcast where id = $1",
        podcast_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("the podcast exists")
}

async fn episode_count(pool: &PgPool, podcast_id: PodcastId) -> i64 {
    sqlx::query_scalar!(
        r#"select count(*) as "count!" from episode where podcast_id = $1"#,
        podcast_id.as_uuid()
    )
    .fetch_one(pool)
    .await
    .expect("episodes are countable")
}

async fn alias_owner(pool: &PgPool, url: &str) -> Option<PodcastId> {
    let normalized = normalize_feed_url(url).expect("test URLs are valid");
    sqlx::query_scalar!(
        "select podcast_id from podcast_alias where normalized_url = $1",
        normalized.normalized()
    )
    .fetch_optional(pool)
    .await
    .expect("aliases are readable")
    .map(PodcastId::from_uuid)
}

#[track_caller]
fn assert_next_poll_in(row: &PollRow, min: Duration, max: Duration) {
    let next = row.next_poll_in.expect("the next poll is scheduled");
    // A second of slack covers the time between the poll and this read.
    assert!(
        next > min.as_secs_f64() - 1.0 && next <= max.as_secs_f64() + 1.0,
        "next poll in {next} s, expected {min:?} to {max:?}"
    );
}

const MINUTE: Duration = Duration::from_secs(60);

// ==============================================================================
// Claiming
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn claim_takes_a_lease_on_the_due_feed(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("lease")).await;

    let claimed = claim(&pool, podcast_id).await;
    assert_eq!(claimed.status, PodcastStatus::Pending);
    assert_eq!(claimed.feed_url, feed_url("lease"));
    assert_next_poll_in(&poll_row(&pool, podcast_id).await, 15 * MINUTE, 15 * MINUTE);

    let again = claim_due(&pool).await.expect("the claim query runs");
    assert!(again.is_none(), "a leased feed isn't due: {again:?}");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn crashed_worker_lease_expires(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("crashed")).await;
    script("crashed", vec![Reply::feed(daily_feed())]);

    // A worker claims the feed and dies without polling it.
    drop(claim(&pool, podcast_id).await);
    sqlx::query!(
        "update feed_poll set next_poll_at = next_poll_at - interval '15 minutes'
         where podcast_id = $1",
        podcast_id.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("the poll row exists");

    let claimed = claim(&pool, podcast_id).await;
    poller(&pool)
        .poll_once(claimed)
        .await
        .expect("the poll runs");
    assert_eq!(podcast_row(&pool, podcast_id).await.status, "ok");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn feeds_parked_at_infinity_are_never_claimed(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("parked")).await;
    sqlx::query!(
        "update feed_poll set next_poll_at = 'infinity' where podcast_id = $1",
        podcast_id.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("the poll row exists");

    let claimed = claim_due(&pool).await.expect("the claim query runs");
    assert!(claimed.is_none(), "{claimed:?}");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn unsubscribe_during_a_poll_keeps_the_feed_parked(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("unsubscribed")).await;
    script("unsubscribed", vec![Reply::feed(daily_feed())]);
    let claimed = claim(&pool, podcast_id).await;
    // The last subscriber leaves while the worker holds the lease.
    sqlx::query!(
        "update feed_poll set next_poll_at = 'infinity' where podcast_id = $1",
        podcast_id.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("the poll row exists");

    poller(&pool)
        .poll_once(claimed)
        .await
        .expect("the poll runs");

    assert_eq!(poll_row(&pool, podcast_id).await.next_poll_in, None);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn concurrent_claims_take_different_feeds(pool: PgPool) {
    let first = common::create_podcast(&pool, &feed_url("concurrent-1")).await;
    let second = common::create_podcast(&pool, &feed_url("concurrent-2")).await;

    // A transaction holding one row's lock, like a worker mid-claim, makes
    // the other claim skip to the next due row.
    let mut tx = pool.begin().await.expect("a transaction starts");
    sqlx::query!(
        "select podcast_id from feed_poll where podcast_id = $1 for update",
        first.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await
    .expect("the poll row exists");
    let claimed = claim(&pool, second).await;
    assert_eq!(claimed.podcast_id, second);
    tx.rollback().await.expect("the transaction rolls back");
}

// ==============================================================================
// Fetch outcomes
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn ok_feed_is_ingested_and_scheduled(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("ok")).await;
    script(
        "ok",
        vec![
            Reply::feed(daily_feed())
                .header(header::ETAG, "\"v1\"")
                .header(header::LAST_MODIFIED, "Sat, 24 Oct 2026 10:00:00 GMT"),
        ],
    );

    let outcome = poll(&poller(&pool), &pool, podcast_id).await;

    assert!(matches!(outcome, PollOutcome::Fetched(_)), "{outcome:?}");
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(
        (podcast.status.as_str(), podcast.status_message),
        ("ok", None)
    );
    assert_eq!(episode_count(&pool, podcast_id).await, 5);
    let row = poll_row(&pool, podcast_id).await;
    assert_eq!(row.etag.as_deref(), Some("\"v1\""));
    assert_eq!(
        row.last_modified.as_deref(),
        Some("Sat, 24 Oct 2026 10:00:00 GMT")
    );
    assert_eq!(row.consecutive_errors, 0);
    assert!(row.polled);
    assert_next_poll_in(&row, POLL_INTERVAL, POLL_INTERVAL.mul_f64(1.1));
    let user_agent = &requests("ok")[0][header::USER_AGENT];
    assert!(
        user_agent
            .to_str()
            .expect("an ASCII user agent")
            .ends_with("(+https://pod.example.org)"),
        "{user_agent:?}"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn not_modified_sends_the_etag_and_leaves_the_catalog(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("etag")).await;
    script(
        "etag",
        vec![
            Reply::feed(daily_feed()).header(header::ETAG, "\"v1\""),
            Reply::status(304),
        ],
    );
    let poller = poller(&pool);
    poll(&poller, &pool, podcast_id).await;
    let before = podcast_row(&pool, podcast_id).await;

    let outcome = poll(&poller, &pool, podcast_id).await;

    assert!(matches!(outcome, PollOutcome::NotModified), "{outcome:?}");
    let sent = &requests("etag")[1];
    assert_eq!(sent[header::IF_NONE_MATCH], "\"v1\"");
    assert!(sent.get(header::IF_MODIFIED_SINCE).is_none());
    assert_eq!(podcast_row(&pool, podcast_id).await, before);
    let row = poll_row(&pool, podcast_id).await;
    assert_eq!(row.etag.as_deref(), Some("\"v1\""));
    assert_next_poll_in(&row, POLL_INTERVAL, POLL_INTERVAL.mul_f64(1.1));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn without_an_etag_last_modified_is_sent(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("modified")).await;
    script(
        "modified",
        vec![
            Reply::feed(daily_feed())
                .header(header::LAST_MODIFIED, "Sat, 24 Oct 2026 10:00:00 GMT"),
            Reply::status(304),
        ],
    );
    let poller = poller(&pool);
    poll(&poller, &pool, podcast_id).await;

    poll(&poller, &pool, podcast_id).await;

    let sent = &requests("modified")[1];
    assert_eq!(
        sent[header::IF_MODIFIED_SINCE],
        "Sat, 24 Oct 2026 10:00:00 GMT"
    );
    assert!(sent.get(header::IF_NONE_MATCH).is_none());
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn gone_feed_stops_polling(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("gone")).await;
    script("gone", vec![Reply::feed(daily_feed()), Reply::status(410)]);
    let poller = poller(&pool);
    poll(&poller, &pool, podcast_id).await;

    let outcome = poll(&poller, &pool, podcast_id).await;

    assert!(matches!(outcome, PollOutcome::Gone), "{outcome:?}");
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.status, "gone");
    assert_eq!(
        podcast.status_message.as_deref(),
        Some("Feed returned HTTP 410")
    );
    assert_eq!(episode_count(&pool, podcast_id).await, 5, "episodes stay");
    assert_eq!(poll_row(&pool, podcast_id).await.next_poll_in, None);
    let claimed = claim_due(&pool).await.expect("the claim query runs");
    assert!(claimed.is_none(), "{claimed:?}");
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn retry_after_seconds_delays_the_next_poll(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("retry-seconds")).await;
    script(
        "retry-seconds",
        vec![Reply::status(429).header(header::RETRY_AFTER, "7200")],
    );

    poll(&poller(&pool), &pool, podcast_id).await;

    let row = poll_row(&pool, podcast_id).await;
    assert_eq!(row.consecutive_errors, 1);
    assert_next_poll_in(&row, 120 * MINUTE, 120 * MINUTE);
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(
        podcast.status_message.as_deref(),
        Some("Feed returned HTTP 429")
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn retry_after_date_delays_the_next_poll(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("retry-date")).await;
    let retry_at = SystemTime::now() + 180 * MINUTE;
    script(
        "retry-date",
        vec![Reply::status(503).header(header::RETRY_AFTER, httpdate::fmt_http_date(retry_at))],
    );

    poll(&poller(&pool), &pool, podcast_id).await;

    // HTTP dates have whole seconds, so the delay may come out a second
    // short.
    let row = poll_row(&pool, podcast_id).await;
    assert_next_poll_in(&row, 180 * MINUTE - Duration::from_secs(1), 180 * MINUTE);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn short_retry_after_keeps_the_error_backoff(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("retry-short")).await;
    script(
        "retry-short",
        vec![Reply::status(503).header(header::RETRY_AFTER, "5")],
    );

    poll(&poller(&pool), &pool, podcast_id).await;

    assert_next_poll_in(&poll_row(&pool, podcast_id).await, 30 * MINUTE, 30 * MINUTE);
}

// ==============================================================================
// Errors and status
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn pending_becomes_failing_on_first_error(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("missing")).await;
    script("missing", vec![Reply::status(404)]);

    let outcome = poll(&poller(&pool), &pool, podcast_id).await;

    assert!(matches!(outcome, PollOutcome::Failed(_)), "{outcome:?}");
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.status, "failing");
    assert_eq!(
        podcast.status_message.as_deref(),
        Some("Feed returned HTTP 404")
    );
    assert!(podcast.version > 0, "clients receive the status");
    let row = poll_row(&pool, podcast_id).await;
    assert_eq!(row.consecutive_errors, 1);
    assert!(row.erroring);
    assert_next_poll_in(&row, 30 * MINUTE, 30 * MINUTE);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn body_that_is_not_a_feed_is_an_error(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("html")).await;
    script("html", vec![Reply::feed("<html><body>Hello</body></html>")]);

    poll(&poller(&pool), &pool, podcast_id).await;

    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.status, "failing");
    assert_eq!(
        podcast.status_message.as_deref(),
        Some("Not a podcast feed")
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn internal_address_is_an_error(pool: PgPool) {
    let url = format!("http://10.0.0.1:{}/s/internal", port());
    let podcast_id = common::create_podcast(&pool, &url).await;

    poll(&poller(&pool), &pool, podcast_id).await;

    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.status, "failing");
    assert_eq!(
        podcast.status_message.as_deref(),
        Some("Address not allowed")
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn repeated_errors_back_off_and_recover(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("flaky")).await;
    script(
        "flaky",
        vec![
            Reply::feed(daily_feed()).header(header::ETAG, "\"v1\""),
            Reply::status(500),
            Reply::status(500),
            Reply::status(500),
            Reply::status(304),
        ],
    );
    let poller = poller(&pool);
    poll(&poller, &pool, podcast_id).await;

    // An ok podcast keeps its status through a short run of errors, while
    // the backoff doubles.
    poll(&poller, &pool, podcast_id).await;
    poll(&poller, &pool, podcast_id).await;
    let row = poll_row(&pool, podcast_id).await;
    assert_eq!(row.consecutive_errors, 2);
    assert_next_poll_in(&row, 60 * MINUTE, 60 * MINUTE);
    assert_eq!(podcast_row(&pool, podcast_id).await.status, "ok");

    // After seven days of errors it becomes failing.
    sqlx::query!(
        "update feed_poll set first_error_at = now() - interval '7 days 1 minute'
         where podcast_id = $1",
        podcast_id.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("the poll row exists");
    poll(&poller, &pool, podcast_id).await;
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.status, "failing");
    assert_eq!(
        podcast.status_message.as_deref(),
        Some("Feed returned HTTP 500")
    );
    assert_eq!(poll_row(&pool, podcast_id).await.consecutive_errors, 3);

    // An unchanged feed afterwards is a success.
    poll(&poller, &pool, podcast_id).await;
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(
        (podcast.status.as_str(), podcast.status_message),
        ("ok", None)
    );
    let row = poll_row(&pool, podcast_id).await;
    assert_eq!(row.consecutive_errors, 0);
    assert!(!row.erroring);
    assert_next_poll_in(&row, POLL_INTERVAL, POLL_INTERVAL.mul_f64(1.1));
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn backoff_is_capped_at_a_day(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("down")).await;
    script("down", vec![Reply::status(500)]);
    sqlx::query!(
        "update feed_poll set consecutive_errors = 20 where podcast_id = $1",
        podcast_id.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("the poll row exists");

    poll(&poller(&pool), &pool, podcast_id).await;

    assert_next_poll_in(
        &poll_row(&pool, podcast_id).await,
        24 * 60 * MINUTE,
        24 * 60 * MINUTE,
    );
}

// ==============================================================================
// Feed moves
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn permanent_redirect_moves_the_feed(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("old")).await;
    script(
        "old",
        vec![Reply::status(301).header(header::LOCATION, "/s/new")],
    );
    script("new", vec![Reply::feed(daily_feed())]);
    let poller = poller(&pool);

    poll(&poller, &pool, podcast_id).await;

    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.feed_url, feed_url("new"));
    assert_eq!(podcast.status, "ok");
    assert_eq!(alias_owner(&pool, &feed_url("old")).await, Some(podcast_id));
    assert_eq!(alias_owner(&pool, &feed_url("new")).await, Some(podcast_id));

    // The next poll fetches the new URL directly.
    poll(&poller, &pool, podcast_id).await;
    assert_eq!(requests("old").len(), 1);
    assert_eq!(requests("new").len(), 2);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn new_feed_url_moves_an_unchanged_feed(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("announcing")).await;
    script(
        "announcing",
        vec![
            Reply::feed(daily_feed()),
            Reply::feed(daily_feed_moving_to(&feed_url("announced"))),
        ],
    );
    let poller = poller(&pool);
    poll(&poller, &pool, podcast_id).await;
    let before = podcast_row(&pool, podcast_id).await;

    let outcome = poll(&poller, &pool, podcast_id).await;

    assert!(
        matches!(
            outcome,
            PollOutcome::Fetched(pod_server::ingest::IngestOutcome::Unchanged)
        ),
        "the tag isn't part of the feed hash: {outcome:?}"
    );
    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.feed_url, feed_url("announced"));
    assert!(podcast.version > before.version, "clients receive the move");
    assert_eq!(
        alias_owner(&pool, &feed_url("announced")).await,
        Some(podcast_id)
    );
    assert!(
        requests("announced").is_empty(),
        "the move waits for the next poll"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn new_feed_url_naming_the_feed_itself_changes_nothing(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("self")).await;
    script(
        "self",
        vec![Reply::feed(daily_feed_moving_to(&feed_url("self")))],
    );
    let poller = poller(&pool);
    poll(&poller, &pool, podcast_id).await;
    let before = podcast_row(&pool, podcast_id).await;

    poll(&poller, &pool, podcast_id).await;

    assert_eq!(podcast_row(&pool, podcast_id).await, before);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn move_onto_another_podcasts_url_is_ignored(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("mover")).await;
    let other = common::create_podcast(&pool, &feed_url("taken")).await;
    // Only the mover is due.
    sqlx::query!(
        "update feed_poll set next_poll_at = 'infinity' where podcast_id = $1",
        other.as_uuid()
    )
    .execute(&pool)
    .await
    .expect("the poll row exists");
    script(
        "mover",
        vec![Reply::feed(daily_feed_moving_to(&feed_url("taken")))],
    );

    poll(&poller(&pool), &pool, podcast_id).await;

    let podcast = podcast_row(&pool, podcast_id).await;
    assert_eq!(podcast.feed_url, feed_url("mover"));
    assert_eq!(podcast.status, "ok");
    assert_eq!(alias_owner(&pool, &feed_url("taken")).await, Some(other));
}

// ==============================================================================
// Worker loop
// ==============================================================================

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn workers_poll_due_feeds_until_shutdown(pool: PgPool) {
    let podcast_id = common::create_podcast(&pool, &feed_url("worker")).await;
    script("worker", vec![Reply::feed(daily_feed())]);
    let shutdown = CancellationToken::new();
    let mut workers = poller(&pool).spawn(
        NonZeroUsize::new(2).expect("2 is non-zero"),
        shutdown.clone(),
    );

    let mut status = String::new();
    for _ in 0..200 {
        status = podcast_row(&pool, podcast_id).await.status;
        if status == "ok" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(status, "ok", "a worker polled the feed within 10 s");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(result) = workers.join_next().await {
            result.expect("the worker didn't panic");
        }
    })
    .await
    .expect("the workers stop promptly on shutdown");
}
