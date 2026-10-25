//! Per-scope version allocation (spec section 2.1).
//!
//! Every syncable row takes its `version` from a counter on its scope owner,
//! `podcast.version_counter` or `users.version_counter`. A writer locks the
//! owner row as the first statement of its transaction, hands out
//! `counter + 1, counter + 2, ...` in memory as it writes rows, and stores
//! the last one before commit. The lock serializes all writers of a scope,
//! so they commit in version order and a reader that sees version `v` has
//! already seen every lower version in that scope.
//!
//! [`VersionScope`] owns the transaction for the whole of that pattern.
//! Opening it from a pool starts the transaction and makes the lock its
//! first statement, and [`VersionScope::commit`] is the only way to commit,
//! so the counter can't be skipped on the way out. Dropping it rolls the
//! transaction back together with every version it handed out, which is the
//! correct outcome on an error path, so there is no unfinished state to
//! detect.
//!
//! What the type can't stop: a writer that opens a second transaction for
//! scoped rows, or one that writes a row with a version it made up instead
//! of one from [`VersionScope::next_version`].

use color_eyre::eyre::{Result, WrapErr};
use pod_protocol::{PodcastId, UserId};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

/// The owner row whose counter a scope allocates from.
#[derive(Debug, Clone, Copy)]
enum Owner {
    Podcast(PodcastId),
    User(UserId),
}

/// A write transaction holding the lock on one scope's counter.
///
/// Run the scope's writes through [`conn`](Self::conn), give each written
/// row [`next_version`](Self::next_version), and finish with [`commit`](Self::commit).
#[derive(Debug)]
pub struct VersionScope {
    tx: Transaction<'static, Postgres>,
    owner: Owner,
    /// The counter as read under the lock. `commit` compares against it to
    /// skip the write-back when nothing was allocated.
    locked_at: i64,
    counter: i64,
}

impl VersionScope {
    /// Begins a transaction and locks the podcast scope, the one ingest
    /// writes (spec 3.3). Returns `None`, with the transaction rolled back,
    /// if the podcast doesn't exist.
    pub async fn begin_podcast(pool: &PgPool, podcast_id: PodcastId) -> Result<Option<Self>> {
        let mut tx = pool.begin().await.context("begin a podcast write")?;
        let counter = sqlx::query_scalar!(
            "select version_counter from podcast where id = $1 for update",
            podcast_id.as_uuid()
        )
        .fetch_optional(&mut *tx)
        .await
        .context("lock the podcast version counter")?;
        Ok(counter.map(|counter| Self::locked(tx, Owner::Podcast(podcast_id), counter)))
    }

    /// Begins a transaction and locks the user scope, the one a sync
    /// request's mutations write (spec 4.4). Returns `None`, with the
    /// transaction rolled back, if the user doesn't exist.
    pub async fn begin_user(pool: &PgPool, user_id: UserId) -> Result<Option<Self>> {
        let mut tx = pool.begin().await.context("begin a user write")?;
        let counter = sqlx::query_scalar!(
            "select version_counter from users where id = $1 for update",
            user_id.as_uuid()
        )
        .fetch_optional(&mut *tx)
        .await
        .context("lock the user version counter")?;
        Ok(counter.map(|counter| Self::locked(tx, Owner::User(user_id), counter)))
    }

    fn locked(tx: Transaction<'static, Postgres>, owner: Owner, counter: i64) -> Self {
        Self {
            tx,
            owner,
            locked_at: counter,
            counter,
        }
    }

    /// Allocates the next version for a row this transaction writes.
    pub fn next_version(&mut self) -> i64 {
        self.counter += 1;
        self.counter
    }

    /// The last version allocated, or the stored counter if none was.
    pub fn current(&self) -> i64 {
        self.counter
    }

    /// The transaction's connection, for the scope's reads and writes.
    pub fn conn(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    /// Stores the counter on the owner row, if any version was allocated,
    /// and commits.
    pub async fn commit(mut self) -> Result<()> {
        if self.counter != self.locked_at {
            match self.owner {
                Owner::Podcast(podcast_id) => sqlx::query!(
                    "update podcast set version_counter = $2 where id = $1",
                    podcast_id.as_uuid(),
                    self.counter
                ),
                Owner::User(user_id) => sqlx::query!(
                    "update users set version_counter = $2 where id = $1",
                    user_id.as_uuid(),
                    self.counter
                ),
            }
            .execute(&mut *self.tx)
            .await
            .context("store the version counter")?;
        }
        self.tx.commit().await.context("commit the scoped write")
    }
}
