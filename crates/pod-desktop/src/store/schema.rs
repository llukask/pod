//! Schema migrations, tracked in SQLite's `user_version`.
//!
//! `user_version` holds how many entries of [`MIGRATIONS`] the database has
//! applied. Opening runs the ones after that, all in one transaction, so a
//! crash mid-upgrade leaves the database at its old version. Migrations are
//! append-only: an entry never changes once released, because databases in
//! the wild already recorded it as applied.

use color_eyre::eyre::{Result, WrapErr, bail};
use rusqlite::{Connection, TransactionBehavior};

const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_initial.sql"),
    include_str!("migrations/0002_rejected_mutation.sql"),
    include_str!("migrations/0003_artwork.sql"),
    include_str!("migrations/0004_artwork_downscaled.sql"),
    include_str!("migrations/0005_feed_status.sql"),
    include_str!("migrations/0006_show_and_episode_types.sql"),
];

/// Brings the schema up to date and returns how many migrations ran.
pub fn migrate(connection: &mut Connection) -> Result<usize> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("begin the migration transaction")?;
    let applied: u32 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("read the schema version")?;
    let applied = applied as usize;
    let latest = u32::try_from(MIGRATIONS.len()).expect("there are far fewer than 2^32 migrations");
    // A database written by a newer build may have tables or columns this
    // build doesn't know how to keep consistent, so it refuses to touch it.
    if applied > MIGRATIONS.len() {
        bail!(
            "the database has schema version {applied}, newer than this build's {}",
            MIGRATIONS.len()
        );
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        transaction
            .execute_batch(sql)
            .with_context(|| format!("apply migration {}", index + 1))?;
    }
    transaction
        .pragma_update(None, "user_version", latest)
        .context("record the schema version")?;
    transaction
        .commit()
        .context("commit the migration transaction")?;
    Ok(MIGRATIONS.len() - applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Table;

    #[test]
    fn migrations_create_exactly_the_tables_in_the_table_enum() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite opens");
        assert_eq!(
            migrate(&mut connection).expect("migrations apply"),
            MIGRATIONS.len()
        );
        let mut statement = connection
            .prepare("select name from sqlite_schema where type = 'table' order by name")
            .expect("the query is valid");
        let created: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .expect("the query runs")
            .collect::<rusqlite::Result<_>>()
            .expect("names are text");
        let mut known: Vec<&str> = Table::ALL.into_iter().map(Table::name).collect();
        known.sort_unstable();
        assert_eq!(created, known);
    }

    /// Files cached before downscaling are dropped, so the next pass
    /// fetches them again at the cache's size.
    #[test]
    fn artwork_cached_before_downscaling_is_dropped() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite opens");
        for sql in &MIGRATIONS[..3] {
            connection
                .execute_batch(sql)
                .expect("the migration applies");
        }
        connection
            .pragma_update(None, "user_version", 3)
            .expect("user_version is writable");
        connection
            .execute(
                "insert into artwork (url, file, bytes, last_used)
                 values ('https://img.example.com/a.jpg', 'f', 3000000, 0)",
                [],
            )
            .expect("the row inserts");
        migrate(&mut connection).expect("migrations apply");
        let rows: i64 = connection
            .query_row("select count(*) from artwork", [], |row| row.get(0))
            .expect("the count reads");
        assert_eq!(rows, 0);
    }

    /// Rows pulled before the type columns existed dropped the types, so
    /// every podcast scope syncs again from the start (spec 5.1).
    #[test]
    fn adding_the_type_columns_repulls_every_podcast_scope() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite opens");
        for sql in &MIGRATIONS[..5] {
            connection
                .execute_batch(sql)
                .expect("the migration applies");
        }
        connection
            .pragma_update(None, "user_version", 5)
            .expect("user_version is writable");
        connection
            .execute_batch(
                "insert into sync_scope (scope, version, complete) values
                     ('user', 70, 1), ('podcast:a', 90, 1), ('podcast:b', 12, 0);",
            )
            .expect("the rows insert");
        migrate(&mut connection).expect("migrations apply");
        let mut statement = connection
            .prepare("select scope, version, complete from sync_scope order by scope")
            .expect("the query is valid");
        let scopes: Vec<(String, i64, bool)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("the query runs")
            .collect::<rusqlite::Result<_>>()
            .expect("the rows read");
        assert_eq!(
            scopes,
            [
                ("podcast:a".to_owned(), 0, false),
                ("podcast:b".to_owned(), 0, false),
                ("user".to_owned(), 70, true),
            ]
        );
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused() {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite opens");
        connection
            .pragma_update(None, "user_version", MIGRATIONS.len() as u32 + 1)
            .expect("user_version is writable");
        let error = migrate(&mut connection).expect_err("a newer schema is refused");
        assert!(format!("{error}").contains("newer than this build"));
    }
}
