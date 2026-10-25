//! `SqlTimestamp` round-trips millisecond values and floors finer ones, the
//! same rule as `pod_protocol::Timestamp` and Postgres
//! `date_trunc('milliseconds', ...)`.

use pod_protocol::Timestamp;
use pod_server::db::SqlTimestamp;
use sqlx::PgPool;

fn ts(s: &str) -> Timestamp {
    s.parse().expect("test literal is RFC 3339")
}

#[sqlx::test(migrations = false)]
async fn millisecond_values_round_trip(pool: PgPool) {
    let at = SqlTimestamp(ts("2026-10-24T09:12:03.120Z"));
    let read = sqlx::query_scalar!(r#"select $1::timestamptz as "at!: SqlTimestamp""#, at as _)
        .fetch_one(&pool)
        .await
        .expect("the query runs");
    assert_eq!(read, at);
}

#[sqlx::test(migrations = false)]
async fn microsecond_values_floor_to_the_millisecond(pool: PgPool) {
    let row = sqlx::query!(
        r#"select timestamptz '2026-10-24T09:12:03.120999Z' as "a!: SqlTimestamp",
                  timestamptz '1969-12-31T23:59:59.9995Z' as "b!: SqlTimestamp""#
    )
    .fetch_one(&pool)
    .await
    .expect("the query runs");
    assert_eq!(row.a.0, ts("2026-10-24T09:12:03.120Z"));
    assert_eq!(row.b.0.as_epoch_ms(), -1, "pre-epoch values floor too");
}
