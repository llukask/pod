//! The daily housekeeping job (spec 2.3 and 2.4): deletes expired sessions
//! and `applied_mutation` rows older than 30 days.
//!
//! Neither table is synced, so these deletes take no version and clients
//! never see them. [`prune`] is the whole job and takes `now` from the
//! caller; [`run`] only schedules it.

use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr};
use pod_protocol::Timestamp;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::db::SqlTimestamp;

/// How long a replayed mutation id is remembered (spec 2.4).
const APPLIED_MUTATION_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Rows per `applied_mutation` delete. Each batch commits on its own, so a
/// large backlog (the first prune after a long outage) never holds one huge
/// transaction or its row locks.
const DELETE_BATCH: i64 = 10_000;

/// The first prune waits this long after startup, so it doesn't compete
/// with migrations and the poller's first burst.
const STARTUP_DELAY: Duration = Duration::from_secs(60);

const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Rows one [`prune`] deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub applied_mutations: u64,
    pub sessions: u64,
}

/// Deletes sessions that expired by `now` and mutations applied more than
/// 30 days before it.
///
/// "Expired" matches authentication, which accepts a session only while
/// `expires_at > now`. Running it again converges: a second call finds
/// nothing, and a run cut short leaves only rows the next run deletes.
pub async fn prune(pool: &PgPool, now: Timestamp) -> Result<PruneReport> {
    let cutoff = Timestamp::from_epoch_ms(now.as_epoch_ms() - APPLIED_MUTATION_RETENTION_MS)
        .context("compute the applied_mutation cutoff")?;
    let mut report = PruneReport::default();
    // `= any(array(...))` makes Postgres look the batch up by ctid (a TID
    // scan); `ctid in (select ...)` can plan as a join over the whole table.
    loop {
        let deleted = sqlx::query!(
            "delete from applied_mutation where ctid = any(array(
                 select ctid from applied_mutation where applied_at < $1 limit $2))",
            SqlTimestamp(cutoff) as _,
            DELETE_BATCH
        )
        .execute(pool)
        .await
        .context("delete a batch of old applied mutations")?
        .rows_affected();
        report.applied_mutations += deleted;
        if deleted < DELETE_BATCH.unsigned_abs() {
            break;
        }
    }

    // One row per signed-in device keeps this table small, so one statement
    // does.
    report.sessions = sqlx::query!(
        "delete from session where expires_at <= $1",
        SqlTimestamp(now) as _
    )
    .execute(pool)
    .await
    .context("delete expired sessions")?
    .rows_affected();
    Ok(report)
}

/// Runs [`prune`] a minute after startup and then once a day until
/// `shutdown` is cancelled.
///
/// A failed prune is logged and retried the next day. Nothing depends on
/// the deletes happening on time; a late prune only keeps rows a day longer.
pub async fn run(pool: PgPool, shutdown: CancellationToken) {
    let mut delay = STARTUP_DELAY;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(delay) => {}
        }
        match prune(&pool, Timestamp::now()).await {
            Ok(report) => tracing::info!(
                applied_mutations = report.applied_mutations,
                sessions = report.sessions,
                "housekeeping pruned old rows"
            ),
            Err(error) => tracing::error!(?error, "housekeeping prune failed"),
        }
        delay = INTERVAL;
    }
}
