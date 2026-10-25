//! A sync client read from spec 5.2 to 5.5, kept in memory.
//!
//! It holds what a real client's SQLite holds (spec 5.1): the user-state
//! rows, the catalog rows of its podcast scopes, one cursor per scope and an
//! outbox. Pulled user rows go through `pod_protocol::local`'s `merge`; the
//! rest is written here from the spec's pseudo-code, without looking at how
//! the server builds its responses. The only server code it calls is the
//! request entry points in [`Server::sync`], which stand in for
//! `POST /v1/sync`.
//
// Deliberately left out, because the property doesn't depend on them:
// placeholder podcast rows for a pending subscribe (5.5 step 3; the
// comparison only looks at podcast rows with a version), `podcast_device`
// settings (5.5 step 3), the account check on a late response (5.7), the
// 400 / `pull_only` path of 5.3 (the simulation never builds a broken
// envelope) and `rejected: invalid_feed_url` cleanup (every simulated URL is
// valid, so any rejection is reported as a failure instead).

use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, WrapErr, bail};
use pod_protocol::local::{LocalEpisodeState, LocalSetting, LocalSubscription, OutboxRow};
use pod_protocol::{
    DeviceId, Episode, EpisodeArchived, EpisodeId, EpisodePlayed, EpisodePosition, MAX_MUTATIONS,
    Mutation, MutationId, MutationOutcome, Podcast, PodcastCursor, PodcastId, PodcastScope,
    ScopeCursors, ScopeEpisode, SettingSet, Stamp, SubscriptionSet, SyncRequest, SyncResponse,
    Timestamp, UserId, normalize_feed_url,
};
use pod_server::fetch::FetchPolicy;
use pod_server::sync::apply_batch;
use pod_server::sync::pull::{PullTestHooks, pull_with};
use sqlx::PgPool;

// ==============================================================================
// The server, as one request entry point
// ==============================================================================

/// The server side of `POST /v1/sync`, called the way the HTTP handler calls
/// it: push first (skipped for a pull-only request, so it takes no lock),
/// then pull from one snapshot. Going around HTTP lets the test pass
/// [`PullTestHooks`]; the response still makes a JSON round trip so the
/// client only sees what the wire carries.
#[derive(Debug, Clone, Copy)]
pub struct Server<'a> {
    pub pool: &'a PgPool,
    pub policy: &'a FetchPolicy,
    pub hooks: PullTestHooks<'a>,
}

impl Server<'_> {
    pub async fn sync(&self, user_id: UserId, request: &SyncRequest) -> Result<SyncResponse> {
        let mutations = request
            .parse_mutations()
            .expect("the client model only sends mutations with ids");
        let now = Timestamp::now();
        let results = if mutations.is_empty() {
            Vec::new()
        } else {
            apply_batch(self.pool, user_id, &mutations, now, self.policy)
                .await
                .context("apply the pushed mutations")?
        };
        let scopes = match &request.scopes {
            Some(cursors) => Some(
                pull_with(
                    self.pool,
                    user_id,
                    cursors,
                    request.effective_limit(),
                    self.hooks,
                )
                .await
                .context("pull the requested scopes")?,
            ),
            None => None,
        };
        let response = SyncResponse {
            server_time: now,
            results,
            scopes,
        };
        let wire = serde_json::to_value(&response).context("serialize the response")?;
        serde_json::from_value(wire).context("parse the response")
    }
}

// ==============================================================================
// Local state
// ==============================================================================

/// A `sync_scope` row for a podcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeCursor {
    pub version: i64,
    pub complete: bool,
}

/// What one sync request does, beyond the full loop of [`Client::sync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// Outbox batch and every scope: one round of the 5.3 loop.
    PushPull,
    /// No mutations: the request that takes no lock on the server, which is
    /// the case the snapshot rule protects (spec 9).
    PullOnly,
    /// No `scopes` (spec 4.2 allows it).
    PushOnly,
}

/// Counters for the run summary, so a green run also shows what it
/// exercised.
#[derive(Debug, Clone, Default)]
pub struct ClientStats {
    pub requests: u64,
    pub pull_only: u64,
    pub push_only: u64,
    pub mutations_sent: u64,
    pub applied: u64,
    pub superseded: u64,
    pub clamp_rewrites: u64,
    pub merged_rows: u64,
    pub redirects: u64,
    pub outbox_redirects: u64,
    pub catalog_rows: u64,
    pub tombstones: u64,
    pub not_subscribed: u64,
    /// Podcast pages for a scope the same response's reconcile step removed
    /// (a local unsubscribe still in the outbox), which the client drops.
    pub skipped_pages: u64,
}

/// One simulated device of one user.
#[derive(Debug)]
pub struct Client {
    pub name: String,
    pub user_id: UserId,
    pub dev: DeviceId,
    /// Added to the wall clock for every local stamp. A lead over 60 s makes
    /// the server clamp (spec 4.4) and the client rewrite its stamp (5.3).
    pub clock_skew_ms: i64,

    pub subscriptions: BTreeMap<PodcastId, LocalSubscription>,
    pub episode_states: BTreeMap<EpisodeId, LocalEpisodeState>,
    pub settings: BTreeMap<String, LocalSetting>,

    pub user_cursor: i64,
    pub podcast_scopes: BTreeMap<PodcastId, ScopeCursor>,
    pub podcasts: BTreeMap<PodcastId, Podcast>,
    pub episodes: BTreeMap<EpisodeId, Episode>,

    /// In `created_at` order.
    pub outbox: Vec<OutboxRow>,
    pub stats: ClientStats,
}

impl Client {
    pub fn new(name: String, user_id: UserId, dev: DeviceId, clock_skew_ms: i64) -> Self {
        Self {
            name,
            user_id,
            dev,
            clock_skew_ms,
            subscriptions: BTreeMap::new(),
            episode_states: BTreeMap::new(),
            settings: BTreeMap::new(),
            user_cursor: 0,
            podcast_scopes: BTreeMap::new(),
            podcasts: BTreeMap::new(),
            episodes: BTreeMap::new(),
            outbox: Vec::new(),
            stats: ClientStats::default(),
        }
    }

    // ==========================================================================
    // Local writes (spec 5.2)
    // ==========================================================================

    /// Queues `payload`, replacing any queued mutation with the same
    /// coalesce key. Nothing is in flight here: a client runs one request
    /// at a time and never writes locally during one.
    fn enqueue(&mut self, coalesce_key: String, payload: Mutation) {
        self.outbox.retain(|row| row.coalesce_key != coalesce_key);
        self.outbox.push(OutboxRow {
            mutation_id: payload.id(),
            coalesce_key,
            payload,
            created_at: Timestamp::now(),
            attempts: 0,
        });
    }

    /// Subscribes or unsubscribes `feed_url` as entered (5.5 steps 1, 2 and
    /// 4). Subscribing adds the scope at 0 right away.
    pub fn set_subscribed(&mut self, feed_url: &str, subscribed: bool) {
        let url = normalize_feed_url(feed_url).expect("simulated feed URLs are valid");
        let podcast_id = PodcastId::from_feed_url(&url);
        let at = local_at(
            self.clock_skew_ms,
            self.subscriptions
                .get(&podcast_id)
                .map(|row| row.subscribed_at),
        );
        self.subscriptions.insert(
            podcast_id,
            LocalSubscription {
                podcast_id,
                feed_url: url.feed_url().to_owned(),
                subscribed,
                subscribed_at: at,
                subscribed_dev: self.dev,
                // A replaced row never stays local (5.5 step 4), so there is
                // no `replaced_by` to keep.
                replaced_by: None,
            },
        );
        if subscribed {
            self.podcast_scopes
                .entry(podcast_id)
                .or_insert(ScopeCursor {
                    version: 0,
                    complete: false,
                });
        }
        self.enqueue(
            format!("sub:{podcast_id}"),
            Mutation::SubscriptionSet(SubscriptionSet {
                id: MutationId::new_v7(),
                at,
                dev: self.dev,
                podcast_id,
                feed_url: feed_url.to_owned(),
                subscribed,
            }),
        );
    }

    pub fn set_episode_field(&mut self, episode_id: EpisodeId, write: EpisodeWrite) {
        let podcast_id = match (
            self.episodes.get(&episode_id),
            self.episode_states.get(&episode_id),
        ) {
            (Some(episode), _) => episode.podcast_id,
            (None, Some(state)) => state.podcast_id,
            (None, None) => panic!("the simulation only writes episodes the client knows"),
        };
        let dev = self.dev;
        let row = self
            .episode_states
            .entry(episode_id)
            .or_insert_with(|| LocalEpisodeState {
                episode_id,
                podcast_id,
                position_ms: 0,
                position_at: None,
                position_dev: None,
                played: false,
                played_at: None,
                played_dev: None,
                archived: false,
                archived_at: None,
                archived_dev: None,
            });
        let id = MutationId::new_v7();
        let (key, payload) = match write {
            EpisodeWrite::Position(position_ms) => {
                let at = local_at(self.clock_skew_ms, row.position_at);
                (row.position_ms, row.position_at, row.position_dev) =
                    (position_ms, Some(at), Some(dev));
                (
                    format!("position:{episode_id}"),
                    Mutation::EpisodePosition(EpisodePosition {
                        id,
                        at,
                        dev,
                        episode_id,
                        position_ms,
                    }),
                )
            }
            EpisodeWrite::Played(played) => {
                let at = local_at(self.clock_skew_ms, row.played_at);
                (row.played, row.played_at, row.played_dev) = (played, Some(at), Some(dev));
                (
                    format!("played:{episode_id}"),
                    Mutation::EpisodePlayed(EpisodePlayed {
                        id,
                        at,
                        dev,
                        episode_id,
                        played,
                    }),
                )
            }
            EpisodeWrite::Archived(archived) => {
                let at = local_at(self.clock_skew_ms, row.archived_at);
                (row.archived, row.archived_at, row.archived_dev) = (archived, Some(at), Some(dev));
                (
                    format!("archived:{episode_id}"),
                    Mutation::EpisodeArchived(EpisodeArchived {
                        id,
                        at,
                        dev,
                        episode_id,
                        archived,
                    }),
                )
            }
        };
        self.enqueue(key, payload);
    }

    pub fn set_setting(&mut self, key: &str, value: serde_json::Value) {
        let at = local_at(
            self.clock_skew_ms,
            self.settings.get(key).map(|row| row.value_at),
        );
        self.settings.insert(
            key.to_owned(),
            LocalSetting {
                key: key.to_owned(),
                value: value.clone(),
                value_at: at,
                value_dev: self.dev,
            },
        );
        self.enqueue(
            format!("setting:{key}"),
            Mutation::SettingSet(SettingSet {
                id: MutationId::new_v7(),
                at,
                dev: self.dev,
                key: key.to_owned(),
                value,
            }),
        );
    }

    // ==========================================================================
    // The sync loop (spec 5.3)
    // ==========================================================================

    /// Runs the 5.3 loop until the outbox is empty, every scope is complete
    /// and no new scope appeared.
    pub async fn sync(&mut self, server: &Server<'_>, limit: u32, max_batch: usize) -> Result<()> {
        // Each round either drains outbox rows or advances a cursor, so a
        // quiet server ends the loop long before this.
        const MAX_ROUNDS: usize = 10_000;
        for _ in 0..MAX_ROUNDS {
            let settled = self
                .request(server, RequestKind::PushPull, limit, max_batch)
                .await?;
            if settled && self.outbox.is_empty() {
                return Ok(());
            }
        }
        bail!(
            "{}: the sync loop didn't settle in {MAX_ROUNDS} rounds",
            self.name
        )
    }

    /// Sends one request and applies the response. Returns whether every
    /// scope came back complete with no new scope added, the loop's exit
    /// condition besides the outbox.
    ///
    /// A failed request leaves the client untouched, outbox included, the
    /// way a network error does; the next request retries the same
    /// mutation ids.
    pub async fn request(
        &mut self,
        server: &Server<'_>,
        kind: RequestKind,
        limit: u32,
        max_batch: usize,
    ) -> Result<bool> {
        let batch: Vec<OutboxRow> = match kind {
            RequestKind::PullOnly => Vec::new(),
            RequestKind::PushPull | RequestKind::PushOnly => self
                .outbox
                .iter()
                .take(max_batch.min(MAX_MUTATIONS))
                .cloned()
                .collect(),
        };
        let scopes = (kind != RequestKind::PushOnly).then(|| self.cursors());
        let request = SyncRequest {
            mutations: batch
                .iter()
                .map(|row| serde_json::to_value(&row.payload).expect("mutations serialize"))
                .collect(),
            scopes,
            limit: Some(limit),
        };
        self.stats.requests += 1;
        match kind {
            RequestKind::PullOnly => self.stats.pull_only += 1,
            RequestKind::PushOnly => self.stats.push_only += 1,
            RequestKind::PushPull => {}
        }
        self.stats.mutations_sent += batch.len() as u64;
        let response = server
            .sync(self.user_id, &request)
            .await
            .with_context(|| format!("{}: send a sync request", self.name))?;
        self.apply_response(&batch, &response, request.scopes.as_ref())
    }

    /// The request's cursors: the user scope, then podcast scopes that were
    /// complete last round, then incomplete ones, so fresh deltas come
    /// before backfill.
    fn cursors(&self) -> ScopeCursors {
        let ordered = self
            .podcast_scopes
            .iter()
            .filter(|(_, scope)| scope.complete)
            .chain(
                self.podcast_scopes
                    .iter()
                    .filter(|(_, scope)| !scope.complete),
            );
        ScopeCursors {
            user: self.user_cursor,
            podcasts: ordered
                .map(|(&id, scope)| PodcastCursor {
                    id,
                    version: scope.version,
                })
                .collect(),
        }
    }

    /// Applies one response in what a real client runs as one local
    /// transaction, in the order of 5.3's pseudo-code.
    fn apply_response(
        &mut self,
        batch: &[OutboxRow],
        response: &SyncResponse,
        requested: Option<&ScopeCursors>,
    ) -> Result<bool> {
        self.apply_results(batch, response)
            .context("apply the mutation results")?;
        let Some(scopes) = &response.scopes else {
            return Ok(true);
        };
        let user = &scopes.user;

        // Redirects in this page (5.5): X was replaced by Y.
        let redirects: Vec<(PodcastId, PodcastId)> = user
            .subscriptions
            .iter()
            .filter_map(|row| row.replaced_by.map(|y| (row.podcast_id, y)))
            .collect();

        // Pending outbox mutations that name a replaced podcast now name the
        // canonical one, coalesce key included (5.5 step 2).
        for &(x, y) in &redirects {
            for row in &mut self.outbox {
                if let Mutation::SubscriptionSet(m) = &mut row.payload
                    && m.podcast_id == x
                {
                    m.podcast_id = y;
                    row.coalesce_key = format!("sub:{y}");
                    self.stats.outbox_redirects += 1;
                }
            }
        }

        // Merge the user scope (5.4).
        for remote in &user.subscriptions {
            match self.subscriptions.get_mut(&remote.podcast_id) {
                Some(local) => local.merge(remote),
                None => {
                    self.subscriptions.insert(remote.podcast_id, remote.into());
                }
            }
        }
        for remote in &user.episode_states {
            match self.episode_states.get_mut(&remote.episode_id) {
                Some(local) => local.merge(remote),
                None => {
                    self.episode_states.insert(remote.episode_id, remote.into());
                }
            }
        }
        for remote in &user.settings {
            match self.settings.get_mut(&remote.key) {
                Some(local) => local.merge(remote),
                None => {
                    self.settings.insert(remote.key.clone(), remote.into());
                }
            }
        }
        self.stats.merged_rows +=
            (user.subscriptions.len() + user.episode_states.len() + user.settings.len()) as u64;
        self.user_cursor = user.version;

        // Act on every received `replaced_by`, whatever the merge decided
        // (5.5 steps 1 and 4).
        for &(x, y) in &redirects {
            self.stats.redirects += 1;
            let replaced = self
                .subscriptions
                .remove(&x)
                .expect("the merge above inserted or kept X's row");
            match self.subscriptions.get_mut(&y) {
                Some(canonical) => {
                    // Strictly newer only: on an exact tie, Y keeps its
                    // fields. Spec 5.5 says `merge`, whose `wins` gives a
                    // same-device tie to X, but a tie here means one device
                    // sent both mutations and the server clamped them into
                    // one millisecond. The server applied them in batch
                    // order and Y's pulled row already shows the result, so
                    // letting X win undoes a later write (see
                    // `a_clamped_alias_redirect_tie_keeps_the_canonical_row`).
                    // TODO: drop this once spec 5.5 step 1 settles the tie
                    // rule (SESSION.md).
                    let newer = (replaced.subscribed_at, replaced.subscribed_dev)
                        > (canonical.subscribed_at, canonical.subscribed_dev);
                    if newer {
                        canonical.subscribed = replaced.subscribed;
                        canonical.feed_url = replaced.feed_url;
                        canonical.subscribed_at = replaced.subscribed_at;
                        canonical.subscribed_dev = replaced.subscribed_dev;
                    }
                }
                None => {
                    self.subscriptions.insert(
                        y,
                        LocalSubscription {
                            podcast_id: y,
                            replaced_by: None,
                            ..replaced
                        },
                    );
                }
            }
            self.podcast_scopes.remove(&x);
            self.podcasts.remove(&x);
            self.episodes.retain(|_, episode| episode.podcast_id != x);
        }

        // Scopes follow the merged subscription table, not the received
        // rows, so a pulled unsubscribe that lost to a newer local
        // re-subscribe deletes nothing.
        self.reconcile_scopes();

        // Podcast scopes.
        let mut all_complete = user.complete;
        for scope in &scopes.podcasts {
            match scope {
                PodcastScope::NotSubscribed(scope) => {
                    self.stats.not_subscribed += 1;
                    self.podcast_scopes.remove(&scope.id);
                }
                PodcastScope::Ok(changes) => {
                    // A page for a scope the reconcile step just removed: a
                    // local unsubscribe the server hasn't seen yet. Writing
                    // it would bring back catalog rows the scope rule
                    // forbids, so it's dropped (see SESSION.md).
                    let Some(cursor) = self.podcast_scopes.get_mut(&changes.id) else {
                        self.stats.skipped_pages += 1;
                        continue;
                    };
                    *cursor = ScopeCursor {
                        version: changes.version,
                        complete: changes.complete,
                    };
                    all_complete &= changes.complete;
                    if let Some(podcast) = &changes.podcast {
                        self.stats.catalog_rows += 1;
                        self.podcasts.insert(podcast.id, podcast.clone());
                    }
                    for episode in &changes.episodes {
                        self.stats.catalog_rows += 1;
                        match episode {
                            ScopeEpisode::Live(episode) => {
                                self.episodes.insert(episode.id, episode.clone());
                            }
                            ScopeEpisode::Deleted(tombstone) => {
                                self.stats.tombstones += 1;
                                self.episodes.remove(&tombstone.id);
                            }
                        }
                    }
                }
            }
        }

        let requested: BTreeSet<PodcastId> = requested
            .map(|cursors| cursors.podcasts.iter().map(|cursor| cursor.id).collect())
            .unwrap_or_default();
        let new_scopes = self.podcast_scopes.keys().any(|id| !requested.contains(id));
        Ok(all_complete && !new_scopes)
    }

    /// Drops acknowledged outbox rows and copies a clamped `at` into the
    /// field the mutation wrote, if that field still carries the sent stamp
    /// (5.3, 5.4).
    fn apply_results(&mut self, batch: &[OutboxRow], response: &SyncResponse) -> Result<()> {
        for result in &response.results {
            let Some(sent) = batch.iter().find(|row| row.mutation_id == result.id) else {
                bail!("{}: a result for a mutation it didn't send", self.name);
            };
            self.outbox.retain(|row| row.mutation_id != result.id);
            let stored_at = match result.outcome {
                MutationOutcome::Applied { at } => {
                    self.stats.applied += 1;
                    at
                }
                MutationOutcome::Superseded { at } => {
                    self.stats.superseded += 1;
                    at
                }
                MutationOutcome::Rejected { code, .. } => {
                    bail!(
                        "{}: the server rejected {:?} with {code:?}; every simulated mutation is valid",
                        self.name,
                        sent.payload
                    );
                }
            };
            if stored_at != sent.payload.at() {
                self.stats.clamp_rewrites += 1;
                self.rewrite_stamp(&sent.payload, stored_at);
            }
        }
        Ok(())
    }

    fn rewrite_stamp(&mut self, sent: &Mutation, stored_at: Timestamp) {
        let sent_stamp = Some(Stamp {
            at: sent.at(),
            dev: sent.dev(),
        });
        let carries_sent = |at: Option<Timestamp>, dev: Option<DeviceId>| {
            Stamp::from_columns(at, dev) == sent_stamp
        };
        match sent {
            Mutation::SubscriptionSet(m) => {
                if let Some(row) = self.subscriptions.get_mut(&m.podcast_id)
                    && carries_sent(Some(row.subscribed_at), Some(row.subscribed_dev))
                {
                    row.subscribed_at = stored_at;
                }
            }
            Mutation::EpisodePosition(m) => {
                if let Some(row) = self.episode_states.get_mut(&m.episode_id)
                    && carries_sent(row.position_at, row.position_dev)
                {
                    row.position_at = Some(stored_at);
                }
            }
            Mutation::EpisodePlayed(m) => {
                if let Some(row) = self.episode_states.get_mut(&m.episode_id)
                    && carries_sent(row.played_at, row.played_dev)
                {
                    row.played_at = Some(stored_at);
                }
            }
            Mutation::EpisodeArchived(m) => {
                if let Some(row) = self.episode_states.get_mut(&m.episode_id)
                    && carries_sent(row.archived_at, row.archived_dev)
                {
                    row.archived_at = Some(stored_at);
                }
            }
            Mutation::SettingSet(m) => {
                if let Some(row) = self.settings.get_mut(&m.key)
                    && carries_sent(Some(row.value_at), Some(row.value_dev))
                {
                    row.value_at = stored_at;
                }
            }
        }
    }

    /// Every subscribed podcast has a scope (new ones at 0); every other
    /// podcast has no scope, no podcast row and no episodes. Episode states
    /// stay (5.3).
    fn reconcile_scopes(&mut self) {
        let subscribed: BTreeSet<PodcastId> = self
            .subscriptions
            .values()
            .filter(|row| row.subscribed)
            .map(|row| row.podcast_id)
            .collect();
        self.podcast_scopes.retain(|id, _| subscribed.contains(id));
        for &id in &subscribed {
            self.podcast_scopes.entry(id).or_insert(ScopeCursor {
                version: 0,
                complete: false,
            });
        }
        self.podcasts.retain(|id, _| subscribed.contains(id));
        self.episodes
            .retain(|_, episode| subscribed.contains(&episode.podcast_id));
    }
}

/// A local write to one field of an episode state.
#[derive(Debug, Clone, Copy)]
pub enum EpisodeWrite {
    Position(i64),
    Played(bool),
    Archived(bool),
}

/// The stamp for a local write to a field stamped `current`:
/// `max(now, current + 1 ms)`, so a local action always wins locally, even
/// over a pulled stamp from a device whose clock runs ahead (spec 5.2).
fn local_at(clock_skew_ms: i64, current: Option<Timestamp>) -> Timestamp {
    let now = Timestamp::now().as_epoch_ms() + clock_skew_ms;
    let at = current.map_or(now, |current| now.max(current.as_epoch_ms() + 1));
    Timestamp::from_epoch_ms(at).expect("simulated clocks stay near today")
}
