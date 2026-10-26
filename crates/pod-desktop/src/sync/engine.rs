//! The sync loop (spec 5.3): push the outbox and pull every scope, one page
//! per `POST /v1/sync`, until the outbox is empty and every scope complete.
//!
//! One engine runs per process. [`SyncEngine::sync`] holds its async mutex
//! for a whole loop, so two loops never interleave, and logout can take the
//! same mutex to keep a response from landing after it (spec 5.7).
//!
//! Each round is three steps:
//!
//! 1. One store write selects the outbox batch and marks it in flight, so no
//!    local write can coalesce a row away between the two, and reads the
//!    cursors.
//! 2. The request goes out.
//! 3. One store write applies the response ([`super::apply`]) and clears the
//!    in-flight set.
//!
//! The engine publishes what it is doing as an [`EngineState`] on a watch
//! channel, which the sync status reads (spec 5.6).

use std::sync::Arc;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use gpui_kit::Global;
use pod_protocol::local::ScopeKey;
use pod_protocol::{
    ErrorBody, MAX_MUTATIONS, MutationId, PodcastCursor, ScopeCursors, SyncRequest, SyncResponse,
    Timestamp, UserId,
};
use reqwest::StatusCode;
use rusqlite::{OptionalExtension as _, Transaction, params};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard, watch};

use super::SyncHandle;
use super::apply::{Applied, apply_response, log_rejection};
use crate::net::NetworkState;
use crate::store::{Store, Table};

/// Changes per response. The server's default, and enough for a backfill to
/// show progress page by page.
const PAGE_LIMIT: u32 = 500;

/// A batch that fails with `400` this many times is dropped (spec 5.3).
const MAX_ATTEMPTS: i64 = 5;

/// The first retry after a failed request waits this long, and each further
/// failure doubles it up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// How often [`SyncEngine::run`] syncs while nothing else asks it to (spec
/// 5.6, foreground timer).
const TIMER_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Bounds a request that hangs, so the loop backs off instead of holding the
/// mutex forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

// ==============================================================================
// State
// ==============================================================================

/// What the engine is doing, for the sync status (spec 5.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineState {
    /// Not syncing. `last_synced` is when a loop last finished, `None`
    /// before the first one in this process.
    Idle { last_synced: Option<Timestamp> },
    /// A loop is running. `backfill` names the show whose episodes are
    /// loading, when a podcast scope is still incomplete.
    Running { backfill: Option<String> },
    /// The OS reports no connection.
    Offline,
    /// A request failed with a network error, `5xx` or `429`; the next try
    /// is at `retry_at`.
    BackingOff { retry_at: Instant },
    /// The server answered `401`. Local use goes on; signing in again
    /// resumes sync (spec 5.7).
    SignedOut,
}

/// The sync engine. Cloning is cheap and every clone shares one mutex.
#[derive(Clone, Debug)]
pub struct SyncEngine {
    inner: Arc<Inner>,
}

impl Global for SyncEngine {}

#[derive(Debug)]
struct Inner {
    store: Store,
    handle: SyncHandle,
    http: reqwest::Client,
    network: watch::Receiver<NetworkState>,
    state: watch::Sender<EngineState>,
    /// The process-wide sync mutex, and what loops carry from one to the
    /// next. Logout holds it through a [`SyncPause`] (spec 5.7).
    run: Arc<Mutex<RunState>>,
    /// How many [`SyncEngine::pause`] calls are waiting for the mutex. A
    /// loop holding it stops as soon as this is non-zero, dropping its
    /// request.
    pausing: watch::Sender<usize>,
    /// Signalled when a loop ends, for the download reconciler (spec 7.4).
    loop_ended: Notify,
}

/// Holds the sync mutex. No loop runs while it lives.
#[derive(Debug)]
pub struct SyncPause {
    _run: OwnedMutexGuard<RunState>,
}

#[derive(Debug, Default)]
struct RunState {
    /// Requests that failed in a row with a retryable error.
    failures: u32,
    last_synced: Option<Timestamp>,
}

impl SyncEngine {
    pub fn new(
        store: Store,
        handle: SyncHandle,
        network: watch::Receiver<NetworkState>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .context("build the HTTP client")?;
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                handle,
                http,
                network,
                state: watch::channel(EngineState::Idle { last_synced: None }).0,
                run: Arc::default(),
                pausing: watch::channel(0).0,
                loop_ended: Notify::new(),
            }),
        })
    }

    /// The engine's state, for the sync status.
    pub fn state(&self) -> watch::Receiver<EngineState> {
        self.inner.state.subscribe()
    }

    /// Notified each time a sync loop finishes. The download reconciler
    /// waits on this (spec 7.4); until it exists nobody does.
    pub fn loop_ended(&self) -> &Notify {
        &self.inner.loop_ended
    }

    /// The client sync and the other requests to the account's server
    /// share.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    /// What the OS reports about the network now.
    pub fn network(&self) -> NetworkState {
        *self.inner.network.borrow()
    }

    /// Cancels the running loop, dropping its request, and holds the sync
    /// mutex until the returned guard drops, so no response is applied in
    /// between (spec 5.7). Also forgets what the loops carried over: the
    /// backoff and the last sync time belong to the account logout clears.
    pub async fn pause(&self) -> SyncPause {
        self.inner.pausing.send_modify(|pausing| *pausing += 1);
        let mut run = self.inner.run.clone().lock_owned().await;
        self.inner.pausing.send_modify(|pausing| *pausing -= 1);
        *run = RunState::default();
        self.publish(EngineState::Idle { last_synced: None });
        SyncPause { _run: run }
    }

    fn publish(&self, state: EngineState) {
        self.inner.state.send_replace(state);
    }
}

// ==============================================================================
// The loop
// ==============================================================================

/// How one round ended.
#[derive(Debug)]
pub(super) enum Round {
    NoAccount,
    /// Applied; the loop goes on.
    More,
    /// Applied, and the outbox is empty (or the loop is pull-only) and every
    /// scope complete.
    Done,
    AccountChanged,
    /// Applied, but nothing moved: no cursor, no outbox row.
    Stalled,
    Failed(RequestFailure),
}

/// Why a request failed as a whole. The error code set is open, so this goes
/// by HTTP status alone (spec 4).
#[derive(Debug)]
pub(super) enum RequestFailure {
    /// `401`.
    SignedOut,
    /// `400`: the envelope or a mutation id is broken, a client bug.
    BadRequest,
    /// A network error, `5xx`, `429`, or a body that isn't a sync response.
    Retry { retry_after: Option<Duration> },
    /// Any other `4xx`, which retrying won't fix.
    Refused(StatusCode),
}

impl SyncEngine {
    /// Runs the sync loop until there is nothing left to push or pull, or a
    /// request fails, and returns the state it ended in. Waits for a loop
    /// already running to end first.
    ///
    /// `Err` means the local store failed; request failures end in a state.
    pub async fn sync(&self) -> Result<EngineState> {
        let mut run = self.inner.run.lock().await;
        let mut pausing = self.inner.pausing.subscribe();
        let result = tokio::select! {
            // Dropping the loop drops its request. A store write it already
            // handed to the writer thread still lands, and the account check
            // in the apply step (or logout's delete after it) covers that.
            _ = pausing.wait_for(|pausing| *pausing > 0) => None,
            result = self.loop_until_quiet(&mut run) => Some(result),
        };
        self.inner.handle.set_in_flight([]);
        let Some(result) = result else {
            // The pause publishes the state and the guard holder decides
            // what happens next.
            return Ok(EngineState::Idle {
                last_synced: run.last_synced,
            });
        };
        let state = result.as_ref().map_or(
            EngineState::Idle {
                last_synced: run.last_synced,
            },
            Clone::clone,
        );
        self.publish(state);
        self.inner.loop_ended.notify_one();
        result
    }

    async fn loop_until_quiet(&self, run: &mut RunState) -> Result<EngineState> {
        loop {
            self.inner.handle.take_wanted();
            if !self.inner.network.borrow().connected {
                return Ok(EngineState::Offline);
            }
            match self.run_loop(run).await {
                // A local write during the loop asked for a sync. Its row
                // may have missed the last request, so run once more.
                Ok(EngineState::Idle { .. }) if self.inner.handle.wanted().borrow().is_some() => {}
                result => return result,
            }
        }
    }

    async fn run_loop(&self, run: &mut RunState) -> Result<EngineState> {
        let mut pull_only = false;
        let idle = |run: &RunState| EngineState::Idle {
            last_synced: run.last_synced,
        };
        loop {
            match self.round(pull_only).await? {
                Round::More => run.failures = 0,
                Round::Done => {
                    run.failures = 0;
                    run.last_synced = Some(Timestamp::now());
                    return Ok(idle(run));
                }
                Round::NoAccount | Round::AccountChanged => return Ok(idle(run)),
                Round::Stalled => {
                    tracing::error!("a sync response moved no cursor; stopping the loop");
                    return Ok(idle(run));
                }
                Round::Failed(RequestFailure::SignedOut) => return Ok(EngineState::SignedOut),
                Round::Failed(RequestFailure::BadRequest) if !pull_only => {
                    // The batch may hold a mutation the server can't even
                    // key a result by. Pull without it for the rest of the
                    // loop; the next loop tries the batch again.
                    pull_only = true;
                }
                Round::Failed(RequestFailure::BadRequest) => {
                    tracing::error!("the server refused a pull-only sync request");
                    return Ok(idle(run));
                }
                Round::Failed(RequestFailure::Retry { retry_after }) => {
                    if !self.inner.network.borrow().connected {
                        return Ok(EngineState::Offline);
                    }
                    run.failures += 1;
                    let backoff = FIRST_BACKOFF
                        .saturating_mul(1 << (run.failures - 1).min(16))
                        .min(MAX_BACKOFF);
                    let delay = retry_after.map_or(backoff, |after| after.max(backoff));
                    return Ok(EngineState::BackingOff {
                        retry_at: Instant::now() + delay,
                    });
                }
                Round::Failed(RequestFailure::Refused(status)) => {
                    // TODO: surface a refused request in the sync status.
                    // Spec 5.6 has no wording for it yet, and with a
                    // matching server it can't happen.
                    tracing::error!(%status, "the server refused the sync request");
                    return Ok(idle(run));
                }
            }
        }
    }

    async fn round(&self, pull_only: bool) -> Result<Round> {
        let handle = self.inner.handle.clone();
        let prepared = self
            .inner
            .store
            .write(move |transaction| prepare(transaction, pull_only, &handle))
            .await
            .context("select the batch and read the cursors")?;
        let Some(prepared) = prepared else {
            return Ok(Round::NoAccount);
        };
        self.publish(EngineState::Running {
            backfill: prepared.backfill.clone(),
        });

        let response = match self.post(&prepared).await {
            Ok(response) => response,
            Err(failure) => {
                self.inner.handle.set_in_flight([]);
                if matches!(failure, RequestFailure::BadRequest) && !prepared.batch.is_empty() {
                    let batch = prepared.batch;
                    self.inner
                        .store
                        .write(move |transaction| count_attempt(transaction, &batch))
                        .await
                        .context("count a failed attempt on the batch")?;
                }
                return Ok(Round::Failed(failure));
            }
        };

        let handle = self.inner.handle.clone();
        let user_id = prepared.user_id;
        self.inner
            .store
            .write(move |transaction| {
                finish_round(transaction, &handle, user_id, &response, pull_only)
            })
            .await
            .context("apply the sync response")
    }

    async fn post(&self, prepared: &Prepared) -> Result<SyncResponse, RequestFailure> {
        let url = format!("{}/v1/sync", prepared.server_url.trim_end_matches('/'));
        let body = serde_json::to_vec(&prepared.request).expect("a sync request serializes");
        let response = self
            .inner
            .http
            .post(url)
            .bearer_auth(&prepared.token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| {
                tracing::info!(%error, "the sync request failed");
                RequestFailure::Retry { retry_after: None }
            })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()?.trim().parse().ok())
            .map(Duration::from_secs);
        let bytes = response.bytes().await.map_err(|error| {
            tracing::info!(%error, "the sync response broke off");
            RequestFailure::Retry { retry_after: None }
        })?;
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(|error| {
                tracing::warn!(%error, "the sync response doesn't parse");
                RequestFailure::Retry { retry_after: None }
            });
        }
        let code = serde_json::from_slice::<ErrorBody>(&bytes)
            .map(|body| body.error.code)
            .ok();
        tracing::info!(%status, ?code, "the server failed the sync request");
        Err(match status {
            StatusCode::UNAUTHORIZED => RequestFailure::SignedOut,
            StatusCode::BAD_REQUEST => RequestFailure::BadRequest,
            StatusCode::TOO_MANY_REQUESTS => RequestFailure::Retry { retry_after },
            status if status.is_server_error() => RequestFailure::Retry { retry_after: None },
            status => RequestFailure::Refused(status),
        })
    }
}

// ==============================================================================
// Store steps
// ==============================================================================

/// One round's request and what the engine needs to send and apply it.
#[derive(Debug)]
pub(super) struct Prepared {
    server_url: String,
    token: String,
    pub(super) user_id: UserId,
    batch: Vec<MutationId>,
    pub(super) request: SyncRequest,
    backfill: Option<String>,
}

/// Reads the account, selects and marks the batch, and lists the cursors.
/// `None` without an account.
pub(super) fn prepare(
    transaction: &Transaction<'_>,
    pull_only: bool,
    handle: &SyncHandle,
) -> Result<Option<Prepared>> {
    let account = transaction
        .query_row(
            "select server_url, token, user_id from account where id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .context("read the account")?;
    let Some((server_url, token, user_id)) = account else {
        return Ok(None);
    };

    // Oldest first, so the server applies mutations in the order the user
    // made them. The id breaks ties within a millisecond.
    let mut batch = Vec::new();
    let mut mutations = Vec::new();
    if !pull_only {
        let mut statement = transaction
            .prepare(
                "select mutation_id, payload from outbox
                 order by created_at, mutation_id limit ?1",
            )
            .context("prepare the batch query")?;
        let rows = statement
            .query_map([MAX_MUTATIONS as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("select the batch")?;
        for row in rows {
            let (id, payload) = row.context("read an outbox row")?;
            batch.push(id.parse().context("parse an outbox mutation id")?);
            mutations.push(serde_json::from_str(&payload).context("parse an outbox payload")?);
        }
    }
    handle.set_in_flight(batch.iter().copied());

    // The user scope first, then podcast scopes that were complete, so fresh
    // deltas don't wait behind a backfill, then the incomplete ones.
    let mut user = 0;
    let mut podcasts = Vec::new();
    let mut backfill = None;
    let mut statement = transaction
        .prepare("select scope, version, complete from sync_scope order by complete desc, scope")
        .context("prepare the cursor query")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .context("read the cursors")?;
    for row in rows {
        let (scope, version, complete) = row.context("read a cursor")?;
        match scope.parse().context("parse a scope key")? {
            ScopeKey::User => user = version,
            ScopeKey::Podcast(id) => {
                if !complete && backfill.is_none() {
                    backfill = transaction
                        .query_row(
                            "select coalesce(title, local_title) from podcast where id = ?1",
                            [id.to_string()],
                            |row| row.get(0),
                        )
                        .optional()
                        .context("read the backfilling show's title")?
                        .flatten();
                }
                podcasts.push(PodcastCursor { id, version });
            }
        }
    }

    Ok(Some(Prepared {
        server_url,
        token,
        user_id: user_id.parse().context("parse the account's user id")?,
        batch,
        request: SyncRequest {
            mutations,
            scopes: Some(ScopeCursors { user, podcasts }),
            limit: Some(PAGE_LIMIT),
        },
        backfill,
    }))
}

/// Applies the response to the request [`prepare`] made for `user_id`,
/// clears the in-flight set, and decides whether the loop goes on.
pub(super) fn finish_round(
    transaction: &Transaction<'_>,
    handle: &SyncHandle,
    user_id: UserId,
    response: &SyncResponse,
    pull_only: bool,
) -> Result<Round> {
    let before = progress(transaction);
    let applied = apply_response(transaction, user_id, response);
    handle.set_in_flight([]);
    if applied.context("apply the response")? == Applied::AccountChanged {
        return Ok(Round::AccountChanged);
    }
    if loop_is_done(transaction, pull_only).context("check for more work")? {
        return Ok(Round::Done);
    }
    // A matching server always moves a cursor or acknowledges a mutation.
    // Without that, asking again would get the same answer forever.
    let before = before.context("read the cursors before applying")?;
    if progress(transaction).context("read the cursors after applying")? == before {
        return Ok(Round::Stalled);
    }
    Ok(Round::More)
}

/// Counts a `400` against every row of the batch and moves rows that reached
/// [`MAX_ATTEMPTS`] to the rejected log.
fn count_attempt(transaction: &Transaction<'_>, batch: &[MutationId]) -> Result<()> {
    for &mutation_id in batch {
        let id = mutation_id.to_string();
        transaction
            .execute(
                "update outbox set attempts = attempts + 1 where mutation_id = ?1",
                [&id],
            )
            .context("count the attempt")?;
        let exhausted: Option<String> = transaction
            .query_row(
                "select payload from outbox where mutation_id = ?1 and attempts >= ?2",
                params![id, MAX_ATTEMPTS],
                |row| row.get(0),
            )
            .optional()
            .context("check the attempt count")?;
        if let Some(payload) = exhausted {
            tracing::warn!(mutation_id = %id, "dropping a mutation the server keeps refusing");
            transaction
                .execute("delete from outbox where mutation_id = ?1", [&id])
                .context("drop the mutation")?;
            log_rejection(transaction, mutation_id, &payload, "bad_request")
                .context("log the dropped mutation")?;
        }
    }
    Ok(())
}

/// Every cursor and the outbox size, to tell whether a response moved
/// anything.
fn progress(transaction: &Transaction<'_>) -> Result<(Option<String>, i64)> {
    transaction
        .query_row(
            "select (select group_concat(scope || ' ' || version || ' ' || complete, ',')
                     from (select * from sync_scope order by scope)),
                    (select count(*) from outbox)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("read the cursors and the outbox size")
}

/// Whether the loop can stop: nothing left to push (or pushing is off for
/// this loop) and every scope complete. A scope new in this response starts
/// incomplete, so it gets its first page.
fn loop_is_done(transaction: &Transaction<'_>, pull_only: bool) -> Result<bool> {
    let pending: i64 = transaction
        .query_row(
            "select (select count(*) from sync_scope where complete = 0)
                  + (case when ?1 then 0 else (select count(*) from outbox) end)",
            [pull_only],
            |row| row.get(0),
        )
        .context("count incomplete scopes and queued mutations")?;
    Ok(pending == 0)
}

// ==============================================================================
// Triggers
// ==============================================================================

impl SyncEngine {
    /// Syncs at start, then whenever a local write's deadline comes due, the
    /// foreground timer fires (spec 5.6), connectivity goes or returns, or a
    /// backoff ends. Runs until the [`SyncHandle`] is gone.
    ///
    /// During a backoff only its end or a connectivity change wake the
    /// engine, so writes don't hammer a failing server. While signed out
    /// only a change to the account row does: a sign-in, or logout.
    pub async fn run(self) {
        let mut wanted = self.inner.handle.wanted();
        let mut network = self.inner.network.clone();
        // Without a monitor (a closed channel) the state stays as it was.
        let mut network_open = true;
        loop {
            // Subscribed before the sync, so a sign-in that lands while a
            // request is being refused still wakes the engine.
            let mut account = self.inner.store.subscribe(Table::Account.into());
            let state = self.sync().await.unwrap_or_else(|error| {
                tracing::error!(error = format!("{error:#}"), "sync failed locally");
                EngineState::Idle { last_synced: None }
            });
            if state == EngineState::SignedOut {
                loop {
                    tokio::select! {
                        _ = account.changed() => break,
                        changed = wanted.changed() => {
                            if changed.is_err() {
                                return;
                            }
                        }
                    }
                }
                continue;
            }
            let timer = Instant::now() + TIMER_INTERVAL;
            loop {
                let wake = match state {
                    EngineState::BackingOff { retry_at } => retry_at,
                    _ => wanted
                        .borrow_and_update()
                        .map_or(timer, |deadline| deadline.min(timer)),
                };
                tokio::select! {
                    () = tokio::time::sleep_until(wake.into()) => break,
                    changed = wanted.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    changed = network.changed(), if network_open => {
                        network_open = changed.is_ok();
                        let connected = network.borrow_and_update().connected;
                        // A lost connection shows in the status at once; the
                        // loop ends offline without a request.
                        let lost = !connected && state != EngineState::Offline;
                        let regained = connected
                            && matches!(state, EngineState::Offline | EngineState::BackingOff { .. });
                        if network_open && (lost || regained) {
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
impl SyncEngine {
    /// Publishes `state` as if a loop had ended in it, for UI tests.
    pub(crate) fn show_state(&self, state: EngineState) {
        self.publish(state);
    }
}
