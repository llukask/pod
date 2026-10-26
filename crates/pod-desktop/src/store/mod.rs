//! The local store: one SQLite file holding the catalog mirror, user state,
//! the outbox, sync cursors and download state (spec 5.1, 7.1). The UI reads
//! only from here.
//!
//! # Threads
//!
//! SQLite calls block, and `rusqlite` connections are `Send` but not `Sync`,
//! so each connection lives on a thread of its own and takes work as boxed
//! closures over a channel:
//!
//! - `pod-db-write` owns the one writer connection. The sync engine,
//!   downloads and the player write through [`Store::write`], one
//!   transaction per call, in the order the calls arrive.
//! - `pod-db-read` owns a read-only connection. [`Store::read`] runs a query
//!   there. The UI's queries go through it ([`live::observe_tables`]), so the UI
//!   thread only awaits a reply and never does disk IO itself.
//!
//! The database is in WAL mode, so a read never waits for a write and sees
//! the last committed state. Replies come back over `tokio::sync::oneshot`,
//! which any executor can await: tokio tasks and GPUI's foreground executor
//! alike.
//!
//! # Change events
//!
//! Every committed write transaction publishes the set of tables it touched
//! on a broadcast channel. [`Store::subscribe`] filters that to the tables a
//! query reads, and [`live::observe_tables`] re-runs a GPUI entity's query when one
//! of them changes. That stands in for Room's observable queries (design.md,
//! "Desktop client").

pub mod live;
mod schema;
mod table;

use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use color_eyre::eyre::{OptionExt as _, Result, WrapErr, ensure};
use gpui_kit::Global;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use tokio::sync::{broadcast, oneshot};

pub use table::{Table, TableSet};

/// How many change events a subscriber can fall behind before it lags. A
/// lagging subscriber treats every table as changed, so this only trades
/// memory for fewer spurious re-runs.
const CHANGE_CAPACITY: usize = 256;

/// How long a statement waits on a lock before failing with `SQLITE_BUSY`.
/// There is one writer, so this only matters for a checkpoint that briefly
/// holds the WAL, or another process opening the same file.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

type WriteJob = Box<dyn FnOnce(&mut Writer) + Send>;
type ReadJob = Box<dyn FnOnce(&mut Connection) + Send>;

/// A handle to the local store. Cloning is cheap; the connections close once
/// every clone is gone. Registered as a GPUI global, so entities reach it
/// with `cx.global::<Store>()`.
#[derive(Clone, Debug)]
pub struct Store {
    writes: mpsc::Sender<WriteJob>,
    reads: mpsc::Sender<ReadJob>,
    changes: broadcast::Sender<TableSet>,
}

impl Global for Store {}

// ==============================================================================
// Opening
// ==============================================================================

impl Store {
    /// Opens the database at `path`, creating it and its directory if
    /// needed, and brings the schema up to date. Blocks on disk IO, so call
    /// it before the UI starts or off the UI thread.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).context("create the database directory")?;
        }
        // The writer opens first: it creates the file, switches it to WAL
        // and migrates, all of which the read-only connection can't do.
        let (connection, applied) = open_writer(path).context("open the writer connection")?;
        tracing::info!(path = %path.display(), applied, "opened the local store");
        let reader = open_reader(path).context("open the read connection")?;

        let (changes, _) = broadcast::channel(CHANGE_CAPACITY);
        let writer = Writer::new(connection, changes.clone())
            .context("install the change hook on the writer connection")?;

        let (writes, write_jobs) = mpsc::channel::<WriteJob>();
        thread::Builder::new()
            .name("pod-db-write".into())
            .spawn(move || {
                let mut writer = writer;
                // Ends once every `Store` clone, and with it every sender,
                // is gone.
                // TODO: a closure that panics takes the thread down and every
                // later call fails with "has stopped". Catch the unwind if a
                // caller ever needs to survive its own panics.
                for job in write_jobs {
                    job(&mut writer);
                }
            })
            .context("spawn the writer thread")?;

        let (reads, read_jobs) = mpsc::channel::<ReadJob>();
        thread::Builder::new()
            .name("pod-db-read".into())
            .spawn(move || {
                let mut reader = reader;
                for job in read_jobs {
                    job(&mut reader);
                }
            })
            .context("spawn the reader thread")?;

        Ok(Store {
            writes,
            reads,
            changes,
        })
    }
}

/// Opens the writer connection with the pragmas every connection to the file
/// relies on, and migrates. Returns the connection and how many migrations
/// ran.
fn open_writer(path: &Path) -> Result<(Connection, usize)> {
    let mut connection = Connection::open(path).context("open the database file")?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .context("set the busy timeout")?;
    // WAL is a property of the file, so setting it here covers the reader
    // too. It lets reads run alongside a write.
    let journal_mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))
        .context("switch to WAL")?;
    ensure!(
        journal_mode == "wal",
        "SQLite kept journal mode {journal_mode}"
    );
    // NORMAL is durable against application crashes in WAL mode; a power
    // loss can lose the last transactions but never corrupts the file. The
    // outbox makes that loss visible to nobody but this device.
    connection
        .pragma_update(None, "synchronous", "normal")
        .context("set synchronous to NORMAL")?;
    // The spec's tables declare no foreign keys on purpose (an
    // `episode_state` row can arrive before its episode, spec 5.1). This is
    // on so any later one is enforced.
    connection
        .pragma_update(None, "foreign_keys", true)
        .context("enable foreign keys")?;
    let applied = schema::migrate(&mut connection).context("migrate the schema")?;
    Ok((connection, applied))
}

/// Opens a read-only connection, so a query can't write past the change
/// events.
fn open_reader(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("open the database file read-only")?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .context("set the busy timeout")?;
    Ok(connection)
}

// ==============================================================================
// Writing
// ==============================================================================

/// The writer connection and the tables its open transaction has touched.
struct Writer {
    connection: Connection,
    touched: Arc<Mutex<TableSet>>,
    changes: broadcast::Sender<TableSet>,
}

impl Writer {
    /// The touched tables come from SQLite's update hook, which reports the
    /// table of every row inserted, updated or deleted on this connection.
    /// Collecting them here, rather than having each write declare its
    /// tables, means no write can forget one.
    ///
    /// The hook doesn't see rows that `INSERT OR REPLACE` deletes, but the
    /// insert that replaced them is reported on the same table. SQLite's
    /// truncate optimisation would skip it for an unqualified `DELETE FROM
    /// t`, so the bundled SQLite is built without that optimisation (see
    /// `.cargo/config.toml`), and a test pins that the hook fires there.
    fn new(connection: Connection, changes: broadcast::Sender<TableSet>) -> Result<Writer> {
        let touched = Arc::new(Mutex::new(TableSet::EMPTY));
        let hook_touched = touched.clone();
        connection
            .update_hook(Some(move |_, _: &str, table: &str, _| {
                let mut touched = hook_touched
                    .lock()
                    .expect("no holder of the lock panics while holding it");
                match Table::from_name(table) {
                    Some(table) => touched.insert(table),
                    // A table the enum doesn't name can only be an SQLite
                    // internal one. Invalidating everything is the safe
                    // answer.
                    None => *touched = TableSet::ALL,
                }
            }))
            .context("register the update hook")?;
        Ok(Writer {
            connection,
            touched,
            changes,
        })
    }

    fn take_touched(&self) -> TableSet {
        std::mem::take(
            &mut *self
                .touched
                .lock()
                .expect("no holder of the lock panics while holding it"),
        )
    }

    fn transact<T>(&mut self, write: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        // Whatever an earlier transaction recorded and then rolled back is
        // not a change.
        self.take_touched();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin the write transaction")?;
        // An error drops the transaction, which rolls it back, and the next
        // call clears what it recorded.
        let value = write(&transaction).context("run the write transaction")?;
        transaction
            .commit()
            .context("commit the write transaction")?;
        // A rolled-back savepoint inside the transaction still counts as a
        // change. That costs a spurious re-run at most.
        let touched = self.take_touched();
        if !touched.is_empty() {
            // `send` only fails when nobody is subscribed, which is fine.
            let _ = self.changes.send(touched);
        }
        Ok(value)
    }
}

impl Store {
    /// Runs `write` in one immediate transaction on the writer thread and
    /// commits it if `write` returns `Ok`. Once it has committed, subscribers
    /// of the tables it touched are told, before this returns.
    pub async fn write<T, F>(&self, write: F) -> Result<T>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        self.writes
            .send(Box::new(move |writer: &mut Writer| {
                // The caller may have stopped waiting; the write stands.
                let _ = reply.send(writer.transact(write));
            }))
            .ok()
            .ok_or_eyre("the writer thread has stopped")?;
        result.await.context("wait for the writer thread")?
    }

    /// Runs `read` on the read thread, inside one read transaction, so a
    /// query over several tables sees a single snapshot.
    pub async fn read<T, F>(&self, read: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        self.reads
            .send(Box::new(move |connection: &mut Connection| {
                let run = || {
                    // Dropping a read transaction rolls it back, which for a
                    // read is the same as committing.
                    let transaction = connection
                        .transaction()
                        .context("begin the read transaction")?;
                    read(&transaction).context("run the read transaction")
                };
                let _ = reply.send(run());
            }))
            .ok()
            .ok_or_eyre("the reader thread has stopped")?;
        result.await.context("wait for the reader thread")?
    }
}

// ==============================================================================
// Change events
// ==============================================================================

impl Store {
    /// Subscribes to committed writes that touch any of `tables`. Writes
    /// committed before this call aren't reported.
    pub fn subscribe(&self, tables: TableSet) -> TableChanges {
        TableChanges {
            tables,
            receiver: self.changes.subscribe(),
        }
    }
}

/// A subscription to writes touching some tables.
#[derive(Debug)]
pub struct TableChanges {
    tables: TableSet,
    receiver: broadcast::Receiver<TableSet>,
}

impl TableChanges {
    /// Waits for a committed write that touches a subscribed table and
    /// returns which subscribed tables changed, folding in every event
    /// already queued. A burst of writes thus wakes the subscriber once.
    /// `None` once the store is gone.
    pub async fn changed(&mut self) -> Option<TableSet> {
        loop {
            let changed = match self.receiver.recv().await {
                Ok(changed) => changed,
                // Missed events may have touched anything.
                Err(broadcast::error::RecvError::Lagged(_)) => TableSet::ALL,
                Err(broadcast::error::RecvError::Closed) => return None,
            };
            let relevant = changed.intersection(self.tables);
            if !relevant.is_empty() {
                return Some(relevant.union(self.try_changed().unwrap_or_default()));
            }
        }
    }

    /// The subscribed tables that queued events touched, without waiting.
    /// `None` if no queued event touched any.
    pub fn try_changed(&mut self) -> Option<TableSet> {
        let mut changed = TableSet::EMPTY;
        loop {
            match self.receiver.try_recv() {
                Ok(tables) => changed = changed.union(tables),
                Err(broadcast::error::TryRecvError::Lagged(_)) => changed = TableSet::ALL,
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => break,
            }
        }
        Some(changed.intersection(self.tables)).filter(|relevant| !relevant.is_empty())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::TempDir;

    /// A fresh directory under the repository's `tmp/`, removed on drop.
    pub(crate) fn scratch_dir() -> TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp");
        std::fs::create_dir_all(&base).expect("tmp/ is creatable inside the repository");
        tempfile::Builder::new()
            .prefix("store-")
            .tempdir_in(base)
            .expect("tmp/ is writable")
    }

    /// An in-memory database with the current schema, for testing queries
    /// without the store's threads.
    pub(crate) fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().expect("in-memory SQLite opens");
        schema::migrate(&mut connection).expect("the migrations apply");
        connection
    }

    pub(crate) fn insert_episode_state(
        transaction: &Transaction<'_>,
        episode_id: &str,
    ) -> Result<()> {
        transaction
            .execute(
                "insert into episode_state (episode_id, podcast_id) values (?1, 'p')",
                params![episode_id],
            )
            .context("insert an episode_state row")?;
        Ok(())
    }

    fn count_rows(connection: &Connection, table: Table) -> Result<i64> {
        connection
            .query_row(
                &format!("select count(*) from {}", table.name()),
                [],
                |row| row.get(0),
            )
            .context("count rows")
    }

    #[tokio::test]
    async fn a_write_wakes_subscribers_of_its_tables_only() {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        let mut states = store.subscribe(Table::EpisodeState.into());
        let mut downloads = store.subscribe(Table::Download.into());

        store
            .write(|transaction| insert_episode_state(transaction, "e1"))
            .await
            .expect("the write commits");

        assert_eq!(states.try_changed(), Some(Table::EpisodeState.into()));
        assert_eq!(downloads.try_changed(), None);
        assert_eq!(
            store
                .read(|connection| count_rows(connection, Table::EpisodeState))
                .await
                .expect("the read runs"),
            1
        );
    }

    #[tokio::test]
    async fn a_rolled_back_write_publishes_nothing() {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        let mut states = store.subscribe(TableSet::ALL);

        let error = store
            .write(|transaction| -> Result<()> {
                insert_episode_state(transaction, "e1")?;
                color_eyre::eyre::bail!("the caller changes its mind")
            })
            .await
            .expect_err("the write fails");
        assert!(format!("{error:#}").contains("the caller changes its mind"));
        assert_eq!(states.try_changed(), None);

        // The next write reports only its own table, not the rolled-back one.
        store
            .write(|transaction| {
                transaction
                    .execute(
                        "insert into device_setting (key, value) values ('k', 'v')",
                        [],
                    )
                    .context("insert a device setting")
            })
            .await
            .expect("the write commits");
        assert_eq!(states.try_changed(), Some(Table::DeviceSetting.into()));
        assert_eq!(
            store
                .read(|connection| count_rows(connection, Table::EpisodeState))
                .await
                .expect("the read runs"),
            0
        );
    }

    #[tokio::test]
    async fn deleting_every_row_is_a_change() {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        store
            .write(|transaction| insert_episode_state(transaction, "e1"))
            .await
            .expect("the write commits");
        let mut states = store.subscribe(Table::EpisodeState.into());

        // Logout clears tables like this (spec 5.7).
        store
            .write(|transaction| {
                transaction
                    .execute("delete from episode_state", [])
                    .context("clear episode_state")
            })
            .await
            .expect("the write commits");
        assert_eq!(states.try_changed(), Some(Table::EpisodeState.into()));
    }

    #[tokio::test]
    async fn a_lagging_subscriber_sees_everything_changed() {
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        let mut downloads = store.subscribe(Table::Download.into());
        for index in 0..=CHANGE_CAPACITY {
            store
                .write(move |transaction| insert_episode_state(transaction, &format!("e{index}")))
                .await
                .expect("the write commits");
        }
        assert_eq!(downloads.changed().await, Some(Table::Download.into()));
    }

    #[test]
    fn reopening_an_existing_database_runs_no_migrations() {
        let dir = scratch_dir();
        let path = dir.path().join("pod.db");
        let (_, applied) = open_writer(&path).expect("a new database opens");
        assert!(applied > 0);
        let (connection, applied) = open_writer(&path).expect("the database reopens");
        assert_eq!(applied, 0);
        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal_mode is readable");
        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn the_read_connection_refuses_writes() {
        let dir = scratch_dir();
        let path = dir.path().join("pod.db");
        open_writer(&path).expect("a new database opens");
        let reader = open_reader(&path).expect("the reader opens");
        reader
            .execute(
                "insert into device_setting (key, value) values ('k', 'v')",
                [],
            )
            .expect_err("a read-only connection can't write");
    }
}
