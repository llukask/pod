//! The feed poller (spec 3.1, 3.2 and 3.4).
//!
//! A few workers share the `feed_poll` table as their queue. Each one claims
//! the most overdue feed with [`claim_due`], which leases it for 15 minutes,
//! and hands it to [`Poller::poll_once`], which fetches it through the SSRF
//! guard, writes what changed to the catalog and schedules the next poll.
//! The lease is what keeps a feed whose worker crashed mid-poll from being
//! retried in a tight loop: nothing has to clean up after the crash, the
//! feed simply becomes due again when the lease runs out.
//!
//! Catalog writes (ingest, status, feed moves) go through
//! [`crate::ingest`] and [`VersionScope`], so clients see them. Poller state
//! (`feed_poll`) is never sent to clients and is written without a version.
//!
//! Logs name the podcast id, never the feed URL, which may carry
//! credentials (spec 3.2).

use std::num::NonZeroUsize;
use std::time::{Duration, SystemTime};

use color_eyre::eyre::{Result, WrapErr, eyre};
use pod_protocol::{
    FeedError, FeedStatus, NormalizedFeedUrl, PodcastId, PodcastStatus, Timestamp,
    normalize_feed_url,
};
use reqwest::StatusCode;
use reqwest::header::{
    ETAG, HeaderMap, HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, RETRY_AFTER,
};
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::db::SqlTimestamp;
use crate::feed::parse_feed;
use crate::fetch::{FetchError, Fetcher};
use crate::ingest::{IngestOutcome, ingest, set_status};
use crate::version::VersionScope;

/// First step of the error backoff, doubled per consecutive error (spec
/// 3.4).
const ERROR_BACKOFF_BASE: Duration = Duration::from_secs(15 * 60);

/// Longest error backoff (spec 3.4).
const ERROR_BACKOFF_MAX: Duration = Duration::from_secs(24 * 60 * 60);

/// Longest delay a `Retry-After` header can impose. The spec says to wait
/// "at least" that long, but a buggy or hostile header asking for a year
/// would otherwise park the feed for good.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long an idle worker waits before looking for due feeds again, as a
/// range so the workers don't query in lockstep.
const IDLE_WAIT_MIN: Duration = Duration::from_secs(1);
const IDLE_WAIT_MAX: Duration = Duration::from_secs(5);

// ==============================================================================
// Claiming work
// ==============================================================================

/// A feed leased to one worker by [`claim_due`].
#[derive(Debug)]
pub struct ClaimedFeed {
    pub podcast_id: PodcastId,
    /// `podcast.feed_url`, which may carry credentials. Never log it.
    pub feed_url: String,
    pub status: PodcastStatus,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub consecutive_errors: i32,
}

/// Claims the most overdue feed and leases it for 15 minutes by moving its
/// `next_poll_at` (spec 3.1). Returns `None` when no feed is due.
///
/// `SKIP LOCKED` lets concurrent workers pass over a row another worker is
/// claiming at that moment instead of waiting for it. A feed parked at
/// `infinity` (gone, or without subscribers) is never due.
pub async fn claim_due(pool: &PgPool) -> Result<Option<ClaimedFeed>> {
    let Some(row) = sqlx::query!(
        "with due as (
             select podcast_id from feed_poll
             where next_poll_at <= now()
             order by next_poll_at
             limit 1
             for update skip locked
         )
         update feed_poll f set next_poll_at = now() + interval '15 minutes'
         from due, podcast p
         where f.podcast_id = due.podcast_id and p.id = f.podcast_id
         returning f.podcast_id, p.feed_url, p.status, f.etag, f.last_modified,
                   f.consecutive_errors"
    )
    .fetch_optional(pool)
    .await
    .context("claim a due feed")?
    else {
        return Ok(None);
    };
    Ok(Some(ClaimedFeed {
        podcast_id: PodcastId::from_uuid(row.podcast_id),
        feed_url: row.feed_url,
        status: row.status.parse().context("read the claimed podcast's status")?,
        etag: row.etag,
        last_modified: row.last_modified,
        consecutive_errors: row.consecutive_errors,
    }))
}

// ==============================================================================
// Poll outcomes
// ==============================================================================

/// What one poll found.
#[derive(Debug)]
pub enum PollOutcome {
    /// The feed answered with a body that parsed. The ingest outcome says
    /// whether anything in it changed.
    Fetched(IngestOutcome),
    /// The feed answered 304.
    NotModified,
    /// The feed answered 410, so polling stopped.
    Gone,
    Failed(PollFailure),
}

/// Why a poll failed. Each one counts as an error for the backoff and the
/// podcast status (spec 3.2).
#[derive(Debug)]
pub enum PollFailure {
    /// An HTTP status that is neither success, 304 nor 410. `retry_after` is
    /// the delay a `Retry-After` header asked for, if any.
    Http {
        status: StatusCode,
        retry_after: Option<Duration>,
    },
    /// The body didn't parse as a feed.
    NotAFeed,
    /// The stored feed URL doesn't parse. Subscribe validates it, so this
    /// only happens to rows written some other way.
    InvalidUrl,
    /// No response at all.
    Fetch(FetchError),
}

impl PollFailure {
    /// The reason clients get (spec 3.2).
    pub fn error(&self) -> FeedError {
        match self {
            PollFailure::Http { status, .. } => FeedError::Http {
                status: status.as_u16(),
            },
            PollFailure::NotAFeed => FeedError::NotAFeed,
            PollFailure::InvalidUrl => FeedError::InvalidUrl,
            PollFailure::Fetch(error) => match error {
                FetchError::UnsupportedScheme(_) | FetchError::MissingHost => FeedError::InvalidUrl,
                FetchError::PortNotAllowed(_) => FeedError::PortNotAllowed,
                FetchError::Blocked(_) => FeedError::AddressNotAllowed,
                FetchError::Resolve(_) => FeedError::HostNotFound,
                FetchError::TooManyRedirects => FeedError::TooManyRedirects,
                FetchError::InvalidRedirect(_) => FeedError::InvalidRedirect,
                FetchError::BodyTooLarge => FeedError::TooLarge,
                FetchError::Timeout => FeedError::Timeout,
                FetchError::Transport(_) => FeedError::ConnectionFailed,
            },
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            PollFailure::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Whether an error leaves the podcast `failing` (spec 3.2). `long_run`
/// says whether errors have lasted seven days.
///
/// A `pending` podcast fails at once, so a mistyped URL shows an error
/// within seconds. A `gone` podcast that someone subscribed to again is
/// polled like a new one. A `failing` podcast keeps failing, with the
/// latest reason.
fn fails_after_error(current: PodcastStatus, long_run: bool) -> bool {
    match current {
        PodcastStatus::Pending | PodcastStatus::Gone | PodcastStatus::Failing => true,
        PodcastStatus::Ok => long_run,
    }
}

/// The run of consecutive errors a failed poll belongs to.
#[derive(Debug, Clone, Copy)]
struct ErrorRun {
    first_error_at: Timestamp,
    /// Whether the run has lasted seven days.
    long_run: bool,
}

// ==============================================================================
// Scheduling
// ==============================================================================

/// Delay after a success: the poll interval plus up to 10 % jitter, so
/// feeds subscribed at the same moment drift apart (spec 3.4). `unit` is a
/// random number in `[0, 1)`.
fn success_delay(poll_interval: Duration, unit: f64) -> Duration {
    poll_interval.mul_f64(1.0 + 0.1 * unit)
}

/// Delay after the `consecutive_errors`-th error in a row (spec 3.4): 30
/// minutes after the first, doubling up to a day.
fn error_delay(consecutive_errors: i32) -> Duration {
    // 2^7 * 15 min already passes 24 h, so larger exponents can't matter.
    let exponent = consecutive_errors.clamp(0, 7) as u32;
    (ERROR_BACKOFF_BASE * 2u32.pow(exponent)).min(ERROR_BACKOFF_MAX)
}

/// Parses `Retry-After` as delay-seconds or an HTTP-date relative to `now`.
/// A date in the past means no delay.
fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    let delay = match value.parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => {
            let at = httpdate::parse_http_date(value).ok()?;
            at.duration_since(now).unwrap_or(Duration::ZERO)
        }
    };
    Some(delay.min(RETRY_AFTER_MAX))
}

/// A uniform random number in `[0, 1)`.
fn random_unit() -> f64 {
    let bits = getrandom::u64().expect("the OS random source is available");
    // The top 53 bits fill an f64 mantissa exactly.
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

// ==============================================================================
// Polling
// ==============================================================================

/// Everything a worker needs. Cheap to clone; clones share the pool and the
/// HTTP connections.
#[derive(Debug, Clone)]
pub struct Poller {
    pool: PgPool,
    fetcher: Fetcher,
    poll_interval: Duration,
}

/// The fetch result, sorted into what the rest of the poll acts on.
#[derive(Debug)]
enum Fetched {
    Body {
        body: bytes::Bytes,
        etag: Option<String>,
        last_modified: Option<String>,
        permanent_redirect: Option<Url>,
    },
    NotModified {
        permanent_redirect: Option<Url>,
    },
    Gone,
    Failed(PollFailure),
}

impl Poller {
    pub fn new(pool: PgPool, fetcher: Fetcher, poll_interval: Duration) -> Poller {
        Poller {
            pool,
            fetcher,
            poll_interval,
        }
    }

    /// Polls one claimed feed end to end: fetch, parse, write the catalog,
    /// schedule the next poll.
    ///
    /// An `Err` means a database write failed partway. The feed keeps its
    /// lease, so it is retried when the lease runs out; every write here is
    /// safe to repeat.
    pub async fn poll_once(&self, feed: ClaimedFeed) -> Result<PollOutcome> {
        let podcast_id = feed.podcast_id;

        // ---------------------------------------------------------------------
        // Fetch conditionally through the SSRF guard.
        // ---------------------------------------------------------------------

        let fetched = match Url::parse(&feed.feed_url) {
            Ok(url) => {
                let response = self.fetcher.get(&url, conditional_headers(&feed)).await;
                sort_response(response, SystemTime::now())
            }
            Err(_) => Fetched::Failed(PollFailure::InvalidUrl),
        };

        match fetched {
            // -----------------------------------------------------------------
            // 200: parse, record feed moves, ingest, schedule normally. A
            // body that isn't a feed is an error like any other, and nothing
            // from it (not even a redirect) is kept.
            // -----------------------------------------------------------------
            Fetched::Body {
                body,
                etag,
                last_modified,
                permanent_redirect,
            } => {
                let parsed = match parse_feed(podcast_id, &feed.feed_url, &body) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        tracing::debug!(%podcast_id, %error, "fetched body is not a feed");
                        return self.fail(&feed, PollFailure::NotAFeed).await;
                    }
                };
                // `itunes:new-feed-url` isn't covered by the feed hash, so it
                // is handled before ingest's "unchanged, stop" check. Neither
                // move is followed in this poll; the next one uses the new
                // URL.
                if let Some(target) = permanent_redirect {
                    self.follow_move(podcast_id, target.as_str()).await?;
                }
                if let Some(target) = &parsed.new_feed_url {
                    self.move_feed(podcast_id, target).await?;
                }
                let outcome = ingest(&self.pool, podcast_id, &parsed)
                    .await
                    .context("ingest the fetched feed")?
                    .ok_or_else(|| eyre!("podcast {podcast_id} has a poll row but no podcast"))?;
                // An applied ingest sets `ok` itself. An unchanged one writes
                // nothing, so a recovering podcast needs the status here.
                if outcome == IngestOutcome::Unchanged {
                    self.recover(&feed).await?;
                }
                self.record_success(podcast_id, Some((etag, last_modified)))
                    .await?;
                tracing::debug!(%podcast_id, ?outcome, "polled feed");
                Ok(PollOutcome::Fetched(outcome))
            }

            // -----------------------------------------------------------------
            // 304: unchanged, but still a success.
            // -----------------------------------------------------------------
            Fetched::NotModified { permanent_redirect } => {
                if let Some(target) = permanent_redirect {
                    self.follow_move(podcast_id, target.as_str()).await?;
                }
                self.recover(&feed).await?;
                self.record_success(podcast_id, None).await?;
                tracing::debug!(%podcast_id, "feed not modified");
                Ok(PollOutcome::NotModified)
            }

            // -----------------------------------------------------------------
            // 410: the feed is gone for good. Episodes stay visible.
            // -----------------------------------------------------------------
            Fetched::Gone => {
                set_status(&self.pool, podcast_id, &FeedStatus::Gone)
                    .await
                    .context("mark the podcast gone")?;
                self.record_gone(podcast_id).await?;
                tracing::info!(%podcast_id, "feed is gone, polling stopped");
                Ok(PollOutcome::Gone)
            }

            Fetched::Failed(failure) => self.fail(&feed, failure).await,
        }
    }

    /// Records a failed poll: backs off, and fails the podcast per spec 3.2.
    ///
    /// A 429 or 503 counts as an error too. Its `Retry-After` only ever
    /// lengthens the backoff, and an `ok` podcast still needs seven days of
    /// errors before it shows as failing.
    async fn fail(&self, feed: &ClaimedFeed, failure: PollFailure) -> Result<PollOutcome> {
        let podcast_id = feed.podcast_id;
        let consecutive_errors = feed.consecutive_errors.saturating_add(1);
        let delay = error_delay(consecutive_errors).max(failure.retry_after().unwrap_or_default());
        let run = self
            .record_error(podcast_id, consecutive_errors, delay)
            .await?;
        let error = failure.error();
        if fails_after_error(feed.status, run.long_run) {
            let status = FeedStatus::Failing {
                failing_since: run.first_error_at,
                error,
            };
            set_status(&self.pool, podcast_id, &status)
                .await
                .context("mark the podcast failing")?;
        }
        tracing::info!(%podcast_id, consecutive_errors, ?error, ?delay, "poll failed");
        Ok(PollOutcome::Failed(failure))
    }

    /// Sets a podcast that isn't `ok` back to `ok` after a successful poll
    /// that ingest didn't write (spec 3.2).
    async fn recover(&self, feed: &ClaimedFeed) -> Result<()> {
        if feed.status != PodcastStatus::Ok {
            set_status(&self.pool, feed.podcast_id, &FeedStatus::Ok)
                .await
                .context("mark the podcast ok")?;
        }
        Ok(())
    }

    // =========================================================================
    // Feed moves
    // =========================================================================

    /// Moves the feed to a permanent redirect target.
    async fn follow_move(&self, podcast_id: PodcastId, target: &str) -> Result<()> {
        match normalize_feed_url(target) {
            Ok(target) => self.move_feed(podcast_id, &target).await,
            // The fetch guard only follows http(s) URLs, so this is
            // unexpected but harmless: keep polling the old URL.
            Err(error) => {
                tracing::warn!(%podcast_id, %error, "ignoring an unusable redirect target");
                Ok(())
            }
        }
    }

    /// Points the podcast at `target` (spec 1): records the old and new
    /// normalized URLs as aliases and stores the new `feed_url`.
    ///
    /// If `target` is already an alias of a different podcast, the move is
    /// skipped and the podcast keeps its URL, since taking over another
    /// podcast's alias would silently redirect its subscribers' subscribes.
    // TODO: decide how to merge two podcasts that turn out to be the same
    // feed (spec gap, see SESSION.md).
    async fn move_feed(&self, podcast_id: PodcastId, target: &NormalizedFeedUrl) -> Result<()> {
        let mut scope = VersionScope::begin_podcast(&self.pool, podcast_id)
            .await
            .context("lock the podcast scope for a feed move")?
            .ok_or_else(|| eyre!("podcast {podcast_id} has a poll row but no podcast"))?;
        let current = sqlx::query!(
            "select feed_url, version from podcast where id = $1",
            podcast_id.as_uuid()
        )
        .fetch_one(scope.conn())
        .await
        .context("read the current feed URL")?;
        // Many feeds name their own URL in `itunes:new-feed-url`.
        if current.feed_url == target.feed_url() {
            scope
                .commit()
                .await
                .context("end a feed move to the same URL")?;
            return Ok(());
        }
        let old = normalize_feed_url(&current.feed_url).context("normalize the stored feed URL")?;

        sqlx::query!(
            "insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)
             on conflict do nothing",
            target.normalized(),
            podcast_id.as_uuid()
        )
        .execute(scope.conn())
        .await
        .context("add the new feed URL alias")?;
        let owner = sqlx::query_scalar!(
            "select podcast_id from podcast_alias where normalized_url = $1",
            target.normalized()
        )
        .fetch_one(scope.conn())
        .await
        .context("read the new feed URL's alias owner")?;
        if owner != podcast_id.as_uuid() {
            // Dropping the scope rolls the transaction back.
            tracing::warn!(
                %podcast_id,
                other_podcast_id = %owner,
                "feed moved to a URL another podcast owns, keeping the old URL"
            );
            return Ok(());
        }
        sqlx::query!(
            "insert into podcast_alias (normalized_url, podcast_id) values ($1, $2)
             on conflict do nothing",
            old.normalized(),
            podcast_id.as_uuid()
        )
        .execute(scope.conn())
        .await
        .context("add the old feed URL alias")?;

        // `feed_url` is client-visible, so the move takes a podcast version.
        // A podcast still at version 0 has never been sent to clients
        // (spec 4.4), and its first ingest will carry the new URL.
        let version = if current.version == 0 {
            0
        } else {
            scope.next_version()
        };
        sqlx::query!(
            "update podcast set feed_url = $2, version = $3, updated_at = now()
             where id = $1",
            podcast_id.as_uuid(),
            target.feed_url(),
            version
        )
        .execute(scope.conn())
        .await
        .context("store the new feed URL")?;
        scope.commit().await.context("commit the feed move")?;
        tracing::info!(%podcast_id, "feed moved to a new URL");
        Ok(())
    }

    // =========================================================================
    // Poller state
    // =========================================================================

    // A concurrent unsubscribe of the last subscriber parks the feed at
    // `infinity` while the poll runs (spec 4.4). The schedule writes below
    // leave that alone, or the poll would put the feed back into rotation.

    /// Records a successful poll. `validators` replaces the stored `ETag`
    /// and `Last-Modified` after a 200; a 304 passes `None` to keep them.
    async fn record_success(
        &self,
        podcast_id: PodcastId,
        validators: Option<(Option<String>, Option<String>)>,
    ) -> Result<()> {
        let replace = validators.is_some();
        let (etag, last_modified) = validators.unwrap_or_default();
        let delay = success_delay(self.poll_interval, random_unit());
        sqlx::query!(
            "update feed_poll
             set etag = case when $2 then $3 else etag end,
                 last_modified = case when $2 then $4 else last_modified end,
                 consecutive_errors = 0,
                 first_error_at = null,
                 last_polled_at = date_trunc('milliseconds', now()),
                 next_poll_at = case when next_poll_at = 'infinity' then next_poll_at
                     else date_trunc('milliseconds', now() + make_interval(secs => $5)) end
             where podcast_id = $1",
            podcast_id.as_uuid(),
            replace,
            etag,
            last_modified,
            delay.as_secs_f64()
        )
        .execute(&self.pool)
        .await
        .context("record a successful poll")?;
        Ok(())
    }

    /// Records an error and returns the run of errors it belongs to (spec
    /// 3.2). `first_error_at` marks the start of the run.
    async fn record_error(
        &self,
        podcast_id: PodcastId,
        consecutive_errors: i32,
        delay: Duration,
    ) -> Result<ErrorRun> {
        let row = sqlx::query!(
            r#"update feed_poll
               set consecutive_errors = $2,
                   first_error_at = coalesce(first_error_at, date_trunc('milliseconds', now())),
                   last_polled_at = date_trunc('milliseconds', now()),
                   next_poll_at = case when next_poll_at = 'infinity' then next_poll_at
                       else date_trunc('milliseconds', now() + make_interval(secs => $3)) end
               where podcast_id = $1
               returning first_error_at as "first_error_at!: SqlTimestamp",
                         now() - first_error_at >= interval '7 days' as "long_run!""#,
            podcast_id.as_uuid(),
            consecutive_errors,
            delay.as_secs_f64()
        )
        .fetch_one(&self.pool)
        .await
        .context("record a failed poll")?;
        Ok(ErrorRun {
            first_error_at: row.first_error_at.0,
            long_run: row.long_run,
        })
    }

    /// Parks a gone feed at `infinity`. A later subscribe makes it due again
    /// (spec 4.4).
    async fn record_gone(&self, podcast_id: PodcastId) -> Result<()> {
        sqlx::query!(
            "update feed_poll
             set consecutive_errors = 0,
                 first_error_at = null,
                 last_polled_at = date_trunc('milliseconds', now()),
                 next_poll_at = 'infinity'
             where podcast_id = $1",
            podcast_id.as_uuid()
        )
        .execute(&self.pool)
        .await
        .context("park the gone feed")?;
        Ok(())
    }

    // =========================================================================
    // Workers
    // =========================================================================

    /// Starts `workers` workers that poll until `shutdown` is cancelled.
    pub fn spawn(self, workers: NonZeroUsize, shutdown: CancellationToken) -> JoinSet<()> {
        let mut set = JoinSet::new();
        for _ in 0..workers.get() {
            let poller = self.clone();
            let shutdown = shutdown.clone();
            set.spawn(async move { poller.run_worker(&shutdown).await });
        }
        set
    }

    /// Claims and polls feeds one at a time, and waits a little when none is
    /// due.
    ///
    /// Shutdown abandons a poll in flight instead of waiting up to the fetch
    /// timeout. Its open transaction rolls back and its lease makes the feed
    /// due again 15 minutes later.
    async fn run_worker(&self, shutdown: &CancellationToken) {
        loop {
            let polled = tokio::select! {
                () = shutdown.cancelled() => return,
                polled = self.claim_and_poll() => polled,
            };
            if !polled {
                let wait = IDLE_WAIT_MIN + (IDLE_WAIT_MAX - IDLE_WAIT_MIN).mul_f64(random_unit());
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(wait) => {}
                }
            }
        }
    }

    /// Polls one due feed, if any. Returns whether there was one. Errors are
    /// logged rather than returned, so one bad feed or a database blip
    /// doesn't stop the worker.
    async fn claim_and_poll(&self) -> bool {
        let feed = match claim_due(&self.pool).await {
            Ok(Some(feed)) => feed,
            Ok(None) => return false,
            Err(error) => {
                tracing::error!("{error:#}");
                return false;
            }
        };
        let podcast_id = feed.podcast_id;
        if let Err(error) = self.poll_once(feed).await {
            tracing::error!(%podcast_id, "poll failed: {error:#}");
        }
        true
    }
}

/// `If-None-Match` when an `ETag` is stored, else `If-Modified-Since` (spec
/// 3.2). A stored value that isn't a valid header value is left out.
fn conditional_headers(feed: &ClaimedFeed) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let (name, value) = match (&feed.etag, &feed.last_modified) {
        (Some(etag), _) => (IF_NONE_MATCH, etag),
        (None, Some(last_modified)) => (IF_MODIFIED_SINCE, last_modified),
        (None, None) => return headers,
    };
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
    headers
}

/// Sorts a fetch result by status (spec 3.2).
///
/// A permanent redirect only moves the feed when the new location answers
/// with a feed or a 304; a redirect to a page that errors isn't trusted.
fn sort_response(
    response: Result<crate::fetch::FetchResponse, FetchError>,
    now: SystemTime,
) -> Fetched {
    let response = match response {
        Ok(response) => response,
        Err(error) => return Fetched::Failed(PollFailure::Fetch(error)),
    };
    let header = |name| {
        response
            .headers
            .get(name)
            .and_then(|value: &HeaderValue| value.to_str().ok())
            .map(str::to_owned)
    };
    match response.status {
        StatusCode::NOT_MODIFIED => Fetched::NotModified {
            permanent_redirect: response.permanent_redirect,
        },
        StatusCode::GONE => Fetched::Gone,
        status if status.is_success() => Fetched::Body {
            etag: header(ETAG),
            last_modified: header(LAST_MODIFIED),
            body: response.body,
            permanent_redirect: response.permanent_redirect,
        },
        status => {
            // Only 429 and 503 promise a meaningful `Retry-After` (spec 3.2).
            let retry_after = matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            )
            .then(|| header(RETRY_AFTER))
            .flatten()
            .and_then(|value| parse_retry_after(&value, now));
            Fetched::Failed(PollFailure::Http {
                status,
                retry_after,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn error_delay_doubles_up_to_a_day() {
        let delays: Vec<_> = [1, 2, 3, 6, 7, 100, i32::MAX]
            .into_iter()
            .map(error_delay)
            .collect();
        assert_eq!(
            delays,
            [
                30 * MINUTE,
                60 * MINUTE,
                120 * MINUTE,
                960 * MINUTE,
                ERROR_BACKOFF_MAX,
                ERROR_BACKOFF_MAX,
                ERROR_BACKOFF_MAX
            ]
        );
    }

    #[test]
    fn success_delay_adds_up_to_ten_percent() {
        let interval = 15 * MINUTE;
        assert_eq!(success_delay(interval, 0.0), interval);
        assert_eq!(success_delay(interval, 0.5), interval.mul_f64(1.05));
        assert!(success_delay(interval, random_unit()) < interval.mul_f64(1.1));
    }

    #[test]
    fn retry_after_takes_seconds_or_a_date() {
        let now = httpdate::parse_http_date("Sun, 25 Oct 2026 12:00:00 GMT").expect("a valid date");
        let parse = |value| parse_retry_after(value, now);
        assert_eq!(parse("120"), Some(2 * MINUTE));
        assert_eq!(parse(" 0 "), Some(Duration::ZERO));
        assert_eq!(parse("Sun, 25 Oct 2026 13:00:00 GMT"), Some(60 * MINUTE));
        // RFC 850 and asctime dates are valid HTTP-dates too.
        assert_eq!(parse("Sunday, 25-Oct-26 13:00:00 GMT"), Some(60 * MINUTE));
        assert_eq!(parse("Sun Oct 25 13:00:00 2026"), Some(60 * MINUTE));
        assert_eq!(parse("Sun, 25 Oct 2026 11:00:00 GMT"), Some(Duration::ZERO));
        assert_eq!(parse("99999999999"), Some(RETRY_AFTER_MAX));
        assert_eq!(parse("soon"), None);
        assert_eq!(parse("-5"), None);
    }

    #[test]
    fn errors_fail_new_podcasts_at_once_and_ok_ones_after_a_week() {
        use PodcastStatus::*;
        assert!(fails_after_error(Pending, false));
        assert!(fails_after_error(Gone, false));
        assert!(fails_after_error(Failing, false));
        assert!(!fails_after_error(Ok, false));
        assert!(fails_after_error(Ok, true));
    }

    #[test]
    fn each_failure_names_its_reason() {
        let http = PollFailure::Http {
            status: StatusCode::NOT_FOUND,
            retry_after: None,
        };
        assert_eq!(http.error(), FeedError::Http { status: 404 });
        assert_eq!(PollFailure::NotAFeed.error(), FeedError::NotAFeed);
        assert_eq!(
            PollFailure::Fetch(FetchError::Timeout).error(),
            FeedError::Timeout
        );
    }
}
