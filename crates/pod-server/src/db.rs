//! Postgres pool, schema migrations and the timestamp column type (spec
//! section 2).
//!
//! Queries use sqlx's compile-time checked macros against the committed
//! `.sqlx/` offline cache. sqlx is built without its `chrono` and `time`
//! features, so the macros map no Rust type to `timestamptz` on their own.
//! Every timestamp column goes through [`SqlTimestamp`], which only holds
//! whole milliseconds, so no query can store a finer value by accident.

use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, eyre};
use pod_protocol::{FeedStatus, PodcastStatus, Timestamp};
use sqlx::postgres::{PgArgumentBuffer, PgPoolOptions, PgTypeInfo, PgValueRef};
use sqlx::{Decode, Encode, PgPool, Postgres, Type};

use crate::config::DatabaseUrl;

/// The schema migrations in `crates/pod-server/migrations`, embedded at
/// compile time. `main` runs them before serving and `#[sqlx::test]` applies
/// them to each test database.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Pool size. The poller holds at most one connection per worker (default
/// 4), which leaves the rest for HTTP requests.
// TODO: make this a config key once deployments need to tune it against the
// Postgres connection limit.
const MAX_CONNECTIONS: u32 = 10;

/// Opens the connection pool and checks that Postgres is reachable, so a bad
/// URL fails at startup rather than on the first request.
pub async fn connect(url: &DatabaseUrl) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url.expose())
        .await
        .context("connect to Postgres")
}

/// Applies every pending migration. Migrations take a Postgres advisory
/// lock, so two servers starting at once don't both apply them.
pub async fn migrate(pool: &PgPool) -> Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("apply the database migrations")
}

// ==============================================================================
// Timestamps
// ==============================================================================

// The spec stores `timestamptz` truncated to whole milliseconds so values
// round-trip through clients' epoch-ms integers. `pod_protocol::Timestamp`
// already floors to the millisecond, but pod-protocol must stay free of sqlx
// and the orphan rule forbids implementing sqlx's traits for it here. This
// wrapper delegates the wire encoding to `jiff-sqlx` and converts at the
// boundary.
//
// Columns filled by a `default now()` in the DDL (`created_at`,
// `updated_at`, `session.last_seen`, `applied_mutation.applied_at`,
// `feed_poll.next_poll_at`) hold microseconds. Clients never receive or
// compare them, so that is harmless, and decoding floors them anyway.

/// A `timestamptz` value with whole-millisecond precision.
///
/// The query macros don't know this type. Bind it with `as _`
/// (`query!("... $1", played_at as _)`) and read it with a column override
/// (`select played_at as "played_at: SqlTimestamp"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SqlTimestamp(pub Timestamp);

impl From<Timestamp> for SqlTimestamp {
    fn from(timestamp: Timestamp) -> Self {
        Self(timestamp)
    }
}

impl From<SqlTimestamp> for Timestamp {
    fn from(timestamp: SqlTimestamp) -> Self {
        timestamp.0
    }
}

impl Type<Postgres> for SqlTimestamp {
    fn type_info() -> PgTypeInfo {
        <jiff_sqlx::Timestamp as Type<Postgres>>::type_info()
    }
}

impl Encode<'_, Postgres> for SqlTimestamp {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        jiff_sqlx::Timestamp::from(self.0.as_jiff()).encode_by_ref(buf)
    }
}

impl<'r> Decode<'r, Postgres> for SqlTimestamp {
    fn decode(value: PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let timestamp = jiff_sqlx::Timestamp::decode(value)?;
        Ok(Self(Timestamp::from_jiff(timestamp.to_jiff())))
    }
}

// ==============================================================================
// Feed status
// ==============================================================================

// `podcast.status` holds the tag, `failing_since` and `feed_error` (the wire
// `FeedError` as jsonb) the failing variant's fields. The check constraint
// `podcast_failing_carries_reason` keeps the three consistent, so a row that
// doesn't read back is a bug, not bad data.

/// The column values that store `status`.
pub fn feed_status_columns(
    status: &FeedStatus,
) -> (
    &'static str,
    Option<SqlTimestamp>,
    Option<serde_json::Value>,
) {
    let (since, error) = match status {
        FeedStatus::Failing {
            failing_since,
            error,
        } => (
            Some(SqlTimestamp(*failing_since)),
            Some(serde_json::to_value(error).expect("a feed error is plain data")),
        ),
        FeedStatus::Pending | FeedStatus::Ok | FeedStatus::Gone => (None, None),
    };
    (status.kind().as_str(), since, error)
}

/// Reads the three status columns back.
pub fn feed_status(
    status: &str,
    failing_since: Option<SqlTimestamp>,
    feed_error: Option<serde_json::Value>,
) -> Result<FeedStatus> {
    let kind: PodcastStatus = status.parse().context("read the podcast status")?;
    Ok(match kind {
        PodcastStatus::Pending => FeedStatus::Pending,
        PodcastStatus::Ok => FeedStatus::Ok,
        PodcastStatus::Gone => FeedStatus::Gone,
        PodcastStatus::Failing => {
            let (Some(since), Some(error)) = (failing_since, feed_error) else {
                return Err(eyre!("a failing podcast has no reason stored"));
            };
            FeedStatus::Failing {
                failing_since: since.0,
                error: serde_json::from_value(error).context("read the feed error")?,
            }
        }
    })
}
