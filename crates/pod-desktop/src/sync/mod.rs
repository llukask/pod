//! The client sync engine (spec 5): local writes (5.2, [`local_write`]), the
//! sync loop (5.3, [`engine`]) and the response apply step it runs in one
//! transaction per page (5.3 and 5.4, `apply`). [`session`] signs in and
//! out (4.1, 5.7).
//!
//! Local writes and the sync loop meet in two places:
//!
//! - The in-flight set: the outbox rows that are part of the request being
//!   sent. Coalescing must leave those alone, because the response deletes
//!   rows by `mutation_id` and a newer coalesced row has to survive it.
//! - The sync request: when the engine should next run. A write asks for a
//!   sync by a deadline, and the earliest outstanding deadline stands.

mod apply;
pub mod engine;
#[cfg(test)]
mod live_tests;
pub mod local_write;
pub mod session;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gpui_kit::Global;
use pod_protocol::MutationId;
use tokio::sync::watch;

/// What local writes share with the sync engine. Cloning is cheap and every
/// clone sees the same state. Registered as a GPUI global.
#[derive(Clone, Debug)]
pub struct SyncHandle {
    in_flight: Arc<Mutex<HashSet<MutationId>>>,
    /// The deadline by which a sync should start, `None` when nothing asked
    /// for one. A `watch` so the engine can sleep until it changes.
    wanted: watch::Sender<Option<Instant>>,
}

impl Global for SyncHandle {}

impl Default for SyncHandle {
    fn default() -> Self {
        Self {
            in_flight: Arc::default(),
            wanted: watch::channel(None).0,
        }
    }
}

impl SyncHandle {
    /// Asks for a sync to start within `delay`. An earlier outstanding
    /// deadline stands, so a burst of writes shares one request and a 2 s
    /// write isn't held back by a pending 30 s position write. A write made
    /// while a request is in flight leaves a deadline behind, which tells
    /// the loop to run once more (spec 5.3).
    pub fn request_sync(&self, delay: Duration) {
        let deadline = Instant::now() + delay;
        self.wanted.send_if_modified(|wanted| match wanted {
            Some(earlier) if *earlier <= deadline => false,
            _ => {
                *wanted = Some(deadline);
                true
            }
        });
    }

    /// A receiver for the engine to wait on new sync requests with.
    pub fn wanted(&self) -> watch::Receiver<Option<Instant>> {
        self.wanted.subscribe()
    }

    /// Clears the outstanding request and returns its deadline. The engine
    /// calls this as a sync starts, so writes made during it ask again.
    pub fn take_wanted(&self) -> Option<Instant> {
        self.wanted.send_replace(None)
    }

    /// Replaces the set of outbox rows in flight. The engine should select
    /// its batch and mark it inside one [`crate::store::Store::write`], so no
    /// local write can coalesce a row away in between, and clear the set in
    /// the transaction that applies the response.
    pub fn set_in_flight(&self, ids: impl IntoIterator<Item = MutationId>) {
        *self.lock_in_flight() = ids.into_iter().collect();
    }

    fn lock_in_flight(&self) -> MutexGuard<'_, HashSet<MutationId>> {
        self.in_flight
            .lock()
            .expect("no holder of the lock panics while holding it")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_earliest_requested_deadline_stands() {
        let sync = SyncHandle::default();
        let mut wanted = sync.wanted();
        assert_eq!(*wanted.borrow_and_update(), None);

        sync.request_sync(Duration::from_secs(30));
        let lazy = wanted
            .borrow_and_update()
            .expect("the request sets a deadline");
        sync.request_sync(Duration::from_secs(2));
        let prompt = wanted
            .borrow_and_update()
            .expect("the request sets a deadline");
        assert!(prompt < lazy, "a sooner request moves the deadline up");

        sync.request_sync(Duration::from_secs(30));
        assert!(
            !wanted
                .has_changed()
                .expect("the handle keeps the channel open"),
            "a later request leaves the sooner deadline alone"
        );
        assert_eq!(sync.take_wanted(), Some(prompt));
        assert_eq!(*wanted.borrow_and_update(), None);
    }
}
