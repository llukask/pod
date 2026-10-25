//! The daily prune (spec 2.3 and 2.4): expired sessions and
//! `applied_mutation` rows older than 30 days go, everything newer stays.

mod common;

use pod_protocol::{Timestamp, UserId};
use pod_server::db::SqlTimestamp;
use pod_server::housekeeping::{PruneReport, prune};
use sqlx::PgPool;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn now() -> Timestamp {
    Timestamp::from_epoch_ms(1_782_864_000_000).expect("2026 is in range")
}

fn before_now(ms: i64) -> Timestamp {
    Timestamp::from_epoch_ms(now().as_epoch_ms() - ms).expect("2026 is in range")
}

/// Inserts `count` applied mutations for `user_id`, all applied at `at`.
async fn applied_mutations(pool: &PgPool, user_id: UserId, at: Timestamp, count: i32) {
    sqlx::query!(
        "insert into applied_mutation (user_id, mutation_id, result, applied_at)
         select $1, gen_random_uuid(), '{}'::jsonb, $2 from generate_series(1, $3)",
        user_id.as_uuid(),
        SqlTimestamp(at) as _,
        count
    )
    .execute(pool)
    .await
    .expect("the mutations insert");
}

async fn mutation_times(pool: &PgPool) -> Vec<Timestamp> {
    sqlx::query_scalar!(
        r#"select applied_at as "applied_at: SqlTimestamp"
           from applied_mutation order by applied_at"#
    )
    .fetch_all(pool)
    .await
    .expect("applied_mutation is readable")
    .into_iter()
    .map(Timestamp::from)
    .collect()
}

async fn session(pool: &PgPool, user_id: UserId, device_name: &str, expires_at: Timestamp) {
    sqlx::query!(
        "insert into session (token_hash, user_id, device_id, device_name, expires_at)
         values (sha256($1::text::bytea), $2, gen_random_uuid(), $1, $3)",
        device_name,
        user_id.as_uuid(),
        SqlTimestamp(expires_at) as _
    )
    .execute(pool)
    .await
    .expect("the session inserts");
}

async fn session_names(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar!("select device_name from session order by device_name")
        .fetch_all(pool)
        .await
        .expect("session is readable")
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn prune_removes_old_mutations_and_keeps_recent_ones(pool: PgPool) {
    let user_id = common::create_user(&pool, "alice").await;
    applied_mutations(&pool, user_id, before_now(31 * DAY_MS), 2).await;
    applied_mutations(&pool, user_id, before_now(30 * DAY_MS + 1), 1).await;
    applied_mutations(&pool, user_id, before_now(30 * DAY_MS), 1).await;
    applied_mutations(&pool, user_id, before_now(DAY_MS), 1).await;

    let report = prune(&pool, now()).await.expect("the prune runs");

    assert_eq!(
        report,
        PruneReport {
            applied_mutations: 3,
            sessions: 0
        }
    );
    assert_eq!(
        mutation_times(&pool).await,
        [before_now(30 * DAY_MS), before_now(DAY_MS)],
        "exactly 30 days old is not older than 30 days"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn prune_removes_expired_sessions_and_keeps_live_ones(pool: PgPool) {
    let user_id = common::create_user(&pool, "alice").await;
    session(&pool, user_id, "expired long ago", before_now(40 * DAY_MS)).await;
    session(&pool, user_id, "expires now", now()).await;
    session(&pool, user_id, "live", before_now(-1)).await;

    let report = prune(&pool, now()).await.expect("the prune runs");

    assert_eq!(
        report,
        PruneReport {
            applied_mutations: 0,
            sessions: 2
        }
    );
    assert_eq!(
        session_names(&pool).await,
        ["live"],
        "authentication treats expires_at <= now as expired"
    );
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn prune_deletes_more_rows_than_one_batch(pool: PgPool) {
    let user_id = common::create_user(&pool, "alice").await;
    applied_mutations(&pool, user_id, before_now(60 * DAY_MS), 25_001).await;
    applied_mutations(&pool, user_id, before_now(DAY_MS), 1).await;

    let report = prune(&pool, now()).await.expect("the prune runs");

    assert_eq!(report.applied_mutations, 25_001);
    assert_eq!(mutation_times(&pool).await, [before_now(DAY_MS)]);
}

#[sqlx::test(migrator = "pod_server::db::MIGRATOR")]
async fn a_second_prune_finds_nothing(pool: PgPool) {
    let user_id = common::create_user(&pool, "alice").await;
    applied_mutations(&pool, user_id, before_now(31 * DAY_MS), 1).await;
    session(&pool, user_id, "expired", before_now(1)).await;
    prune(&pool, now()).await.expect("the prune runs");

    let report = prune(&pool, now()).await.expect("the prune runs");

    assert_eq!(report, PruneReport::default());
}
