//! Live queries for GPUI entities: the desktop's stand-in for Room's
//! observable queries.

use std::sync::Arc;

use color_eyre::eyre::Result;
use gpui_kit::{Context, Task};
use rusqlite::Connection;

use super::{Store, TableSet};

/// Runs `query` now and again after every committed write that touches one
/// of `tables`, hands each result to `apply` on the entity and notifies it.
///
/// The query runs on the store's read thread, never on the UI thread. Writes
/// that land while a query runs, or while the entity waits its turn on the
/// foreground executor, are folded into one re-run. The returned task owns
/// the subscription: keep it in the entity, and dropping it (or the entity)
/// stops the re-runs. Requires the [`Store`] global.
///
/// ```ignore
/// struct InboxCount {
///     unplayed: Option<i64>,
///     _query: Task<()>,
/// }
///
/// cx.new(|cx| InboxCount {
///     unplayed: None,
///     _query: store::live::observe_tables(
///         cx,
///         Table::EpisodeState.into(),
///         |connection| {
///             connection
///                 .query_row("select count(*) from episode_state where played = 0", [], |row| row.get(0))
///                 .context("count unplayed episodes")
///         },
///         |this, unplayed, _| this.unplayed = Some(unplayed),
///     ),
/// })
/// ```
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the inbox list is the first live query")
)]
pub fn observe_tables<E, T>(
    cx: &mut Context<E>,
    tables: TableSet,
    query: impl Fn(&Connection) -> Result<T> + Send + Sync + 'static,
    apply: impl Fn(&mut E, T, &mut Context<E>) + 'static,
) -> Task<()>
where
    E: 'static,
    T: Send + 'static,
{
    let store = cx.global::<Store>().clone();
    // Subscribing before the first run means a write that commits while it
    // runs still triggers a re-run.
    let mut changes = store.subscribe(tables);
    let query = Arc::new(query);
    cx.spawn(async move |this, cx| {
        loop {
            let query = query.clone();
            match store.read(move |connection| query(connection)).await {
                Ok(value) => {
                    let applied = this.update(cx, |entity, cx| {
                        apply(entity, value, cx);
                        cx.notify();
                    });
                    if applied.is_err() {
                        // The entity is gone.
                        return;
                    }
                }
                // TODO: hand failures to the entity once a view has a way to
                // show them; for now it keeps its last result.
                Err(error) => tracing::warn!(?tables, "live query failed: {error:#}"),
            }
            if changes.changed().await.is_none() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use color_eyre::eyre::WrapErr as _;
    use gpui_kit::{AppContext as _, Entity, TestAppContext};

    use super::*;
    use crate::store::Table;
    use crate::store::tests::{insert_episode_state, scratch_dir};

    #[derive(Debug)]
    struct EpisodeCount {
        count: Option<i64>,
        _query: Task<()>,
    }

    /// Drives the foreground executor until `done` holds. The query replies
    /// arrive from the read thread in real time, so this also waits.
    fn wait_until(
        cx: &mut TestAppContext,
        entity: &Entity<EpisodeCount>,
        done: impl Fn(&EpisodeCount) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            if entity.read_with(cx, |entity, _| done(entity)) {
                return;
            }
            assert!(Instant::now() < deadline, "the live query didn't update");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[gpui_kit::test]
    fn a_live_query_reruns_once_per_burst_of_relevant_writes(cx: &mut TestAppContext) {
        // The store's threads wake the entity's task from outside the test
        // scheduler, which it otherwise reports as non-determinism.
        cx.executor().allow_parking();
        let dir = scratch_dir();
        let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
        cx.update(|cx| cx.set_global(store.clone()));
        // Writes block this thread on a runtime of their own, so the
        // foreground executor can't run in between them.
        let writes = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime builds");

        let runs = Arc::new(AtomicUsize::new(0));
        let query_runs = runs.clone();
        let entity = cx.new(|cx| EpisodeCount {
            count: None,
            _query: observe_tables(
                cx,
                Table::EpisodeState.into(),
                move |connection| {
                    query_runs.fetch_add(1, Ordering::SeqCst);
                    connection
                        .query_row("select count(*) from episode_state", [], |row| row.get(0))
                        .context("count episode states")
                },
                |this: &mut EpisodeCount, count, _| this.count = Some(count),
            ),
        });
        wait_until(cx, &entity, |entity| entity.count == Some(0));
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        for index in 0..5 {
            writes
                .block_on(store.write(move |transaction| {
                    insert_episode_state(transaction, &format!("e{index}"))
                }))
                .expect("the write commits");
        }
        wait_until(cx, &entity, |entity| entity.count == Some(5));
        assert_eq!(runs.load(Ordering::SeqCst), 2, "five writes, one re-run");

        writes
            .block_on(store.write(|transaction| {
                transaction
                    .execute(
                        "insert into download_wish (episode_id, source, created_at) \
                         values ('e1', 'pin', 0)",
                        [],
                    )
                    .context("insert a download wish")
            }))
            .expect("the write commits");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(50));
        cx.run_until_parked();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "a write to another table doesn't re-run the query"
        );
    }
}
