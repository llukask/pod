//! Runner for the state-transition conformance fixtures (`conformance/sync_*.json`,
//! spec section 9), generic over the client's local store.
//!
//! A fixture seeds the local tables, applies one sync response the way the
//! sync loop does (spec 5.3) and compares the tables with the expected ones.
//! A client runs the fixtures against its own store by implementing
//! [`ConformanceStore`]; [`MemoryStore`] is the reference implementation this
//! crate's tests run. `conformance/README.md` documents the file format.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::local::{
    LocalAccount, LocalEpisodeState, LocalSetting, LocalSubscription, OutboxRow, ScopeKey,
    SyncScopeRow,
};
use crate::lww::Stamp;
use crate::{
    DeviceId, EpisodeId, Mutation, MutationOutcome, PodcastId, ScopeChanges, SyncResponse,
    Timestamp,
};

// ==============================================================================
// Fixture format
// ==============================================================================

/// One `conformance/sync_*.json` file: a group of cases on one topic.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncFixtureFile {
    pub description: String,
    pub cases: Vec<SyncCase>,
}

/// One state-transition case.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncCase {
    /// A sentence stating the rule the case pins.
    pub name: String,
    /// Every row of every table the case needs, with all columns that have
    /// no SQL default.
    pub local_before: LocalTables,
    /// A complete `POST /v1/sync` response body.
    pub server_response: SyncResponse,
    /// Expected rows of the tables named, each row naming its primary key and
    /// the columns the case checks (see [`compare_tables`]).
    pub local_after: BTreeMap<String, Vec<serde_json::Map<String, Value>>>,
}

/// The local tables (spec 5.1) the fixtures touch, as complete rows.
///
/// Unknown table names are an error, so a fixture that needs a table no
/// store models yet fails loudly instead of passing on half its data.
// TODO: add `podcast`, `episode` and `podcast_device` with the fixtures for
// podcast scopes and `replaced_by` (spec 5.3, 5.5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTables {
    /// At most one row.
    #[serde(default)]
    pub account: Vec<LocalAccount>,
    #[serde(default)]
    pub subscription: Vec<LocalSubscription>,
    #[serde(default)]
    pub episode_state: Vec<LocalEpisodeState>,
    #[serde(default)]
    pub setting: Vec<LocalSetting>,
    #[serde(default)]
    pub sync_scope: Vec<SyncScopeRow>,
    #[serde(default)]
    pub outbox: Vec<OutboxRow>,
}

/// The primary key column of each table in [`LocalTables`]. `account` has at
/// most one row (`id = 1` in SQLite), which fixtures don't spell out, so
/// its rows match without a key column.
const PRIMARY_KEYS: &[(&str, Option<&str>)] = &[
    ("account", None),
    ("subscription", Some("podcast_id")),
    ("episode_state", Some("episode_id")),
    ("setting", Some("key")),
    ("sync_scope", Some("scope")),
    ("outbox", Some("mutation_id")),
];

// ==============================================================================
// The store a client implements
// ==============================================================================

/// A client's local store, as far as the fixtures see it.
///
/// The trait covers what the current fixtures exercise; it grows with them.
pub trait ConformanceStore {
    type Error: fmt::Display;

    /// Replaces the store's contents with `tables`.
    fn seed(&mut self, tables: &LocalTables) -> Result<(), Self::Error>;

    /// Applies one sync response in one local transaction, as spec 5.3's
    /// `sync()` does after a request returns. The request it answers is the
    /// outbox's mutations named in `response.results`.
    fn apply_sync_response(&mut self, response: &SyncResponse) -> Result<(), Self::Error>;

    /// Every row of every table in [`LocalTables`], in any order.
    fn dump(&self) -> Result<LocalTables, Self::Error>;
}

// ==============================================================================
// Running and comparing
// ==============================================================================

/// Why a case failed: one line per difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseFailure {
    pub file: PathBuf,
    pub case: String,
    pub problems: Vec<String>,
}

impl fmt::Display for CaseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}: {}", self.file.display(), self.case)?;
        for problem in &self.problems {
            writeln!(f, "    {problem}")?;
        }
        Ok(())
    }
}

/// The `sync_*.json` files in `dir`, sorted by name.
pub fn sync_fixture_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|name| name.to_str());
        if name.is_some_and(|name| name.starts_with("sync_") && name.ends_with(".json")) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Runs every case of every `sync_*.json` file in `dir`, each against a fresh
/// store from `new_store`, and returns the failures. Unreadable files fail
/// as a whole.
pub fn run_sync_fixtures<S: ConformanceStore>(
    dir: &Path,
    mut new_store: impl FnMut() -> S,
) -> std::io::Result<Vec<CaseFailure>> {
    let mut failures = Vec::new();
    for path in sync_fixture_paths(dir)? {
        let text = std::fs::read_to_string(&path)?;
        let file: SyncFixtureFile = match serde_json::from_str(&text) {
            Ok(file) => file,
            Err(error) => {
                failures.push(CaseFailure {
                    file: path,
                    case: "(whole file)".into(),
                    problems: vec![format!("doesn't parse as a sync fixture file: {error}")],
                });
                continue;
            }
        };
        for case in &file.cases {
            if let Err(problems) = run_case(&mut new_store(), case) {
                failures.push(CaseFailure {
                    file: path.clone(),
                    case: case.name.clone(),
                    problems,
                });
            }
        }
    }
    Ok(failures)
}

/// Seeds `store`, applies the case's response and compares the result.
pub fn run_case<S: ConformanceStore>(store: &mut S, case: &SyncCase) -> Result<(), Vec<String>> {
    store
        .seed(&case.local_before)
        .map_err(|error| vec![format!("seed failed: {error}")])?;
    store
        .apply_sync_response(&case.server_response)
        .map_err(|error| vec![format!("applying the response failed: {error}")])?;
    let actual = store
        .dump()
        .map_err(|error| vec![format!("dump failed: {error}")])?;
    compare_tables(&actual, &case.local_after)
}

/// Compares the store's tables with a fixture's `local_after`.
///
/// `local_after` is a partial expectation, like the spec's example:
///
/// - Only the tables it names are checked. An empty array asserts the table
///   is empty.
/// - Within a named table the rows must match one to one by primary key: no
///   expected row may be missing and no other row may exist.
/// - Within a row, only the columns it lists are compared, as JSON values in
///   the fixture's encoding. A listed column the table doesn't have is an
///   error, so a typo can't pass silently.
pub fn compare_tables(
    actual: &LocalTables,
    expected: &BTreeMap<String, Vec<serde_json::Map<String, Value>>>,
) -> Result<(), Vec<String>> {
    let actual = match serde_json::to_value(actual) {
        Ok(Value::Object(tables)) => tables,
        other => {
            return Err(vec![format!(
                "tables don't serialize to an object: {other:?}"
            )]);
        }
    };
    let mut problems = Vec::new();
    for (table, expected_rows) in expected {
        let Some(&(_, key)) = PRIMARY_KEYS.iter().find(|(name, _)| name == table) else {
            problems.push(format!("unknown table {table:?}"));
            continue;
        };
        let actual_rows = match actual.get(table) {
            Some(Value::Array(rows)) => rows.as_slice(),
            _ => &[],
        };
        compare_rows(table, key, actual_rows, expected_rows, &mut problems);
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn compare_rows(
    table: &str,
    key: Option<&str>,
    actual_rows: &[Value],
    expected_rows: &[serde_json::Map<String, Value>],
    problems: &mut Vec<String>,
) {
    let key_of = |row: &serde_json::Map<String, Value>| match key {
        Some(key) => row.get(key).map(Value::to_string),
        None => Some("(the row)".to_owned()),
    };
    let mut actual_by_key = BTreeMap::new();
    for row in actual_rows {
        if let Some(row) = row.as_object()
            && let Some(row_key) = key_of(row)
            && actual_by_key.insert(row_key.clone(), row).is_some()
        {
            problems.push(format!("{table} {row_key}: duplicate primary key"));
        }
    }
    let mut expected_keys = BTreeSet::new();
    for expected in expected_rows {
        let Some(row_key) = key_of(expected) else {
            problems.push(format!(
                "{table}: expected row {expected:?} lacks its key column"
            ));
            continue;
        };
        expected_keys.insert(row_key.clone());
        let Some(actual) = actual_by_key.get(&row_key) else {
            problems.push(format!("{table} {row_key}: row missing"));
            continue;
        };
        for (column, want) in expected {
            match actual.get(column) {
                None => problems.push(format!("{table} {row_key}: no column {column:?}")),
                Some(got) if got != want => {
                    problems.push(format!(
                        "{table} {row_key}.{column}: want {want}, got {got}"
                    ));
                }
                Some(_) => {}
            }
        }
    }
    for row_key in actual_by_key.keys() {
        if !expected_keys.contains(row_key) {
            problems.push(format!("{table} {row_key}: unexpected row"));
        }
    }
}

// ==============================================================================
// Reference store
// ==============================================================================

/// An in-memory local store implementing spec 5.3's response handling for the
/// parts the fixtures cover. The fixtures are the contract; this is the Rust
/// reading of it.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    tables: LocalTables,
}

/// A response the reference store doesn't handle yet. Failing on these keeps a
/// fixture that needs them from passing by accident.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unsupported {
    #[error("podcast scopes in the response (spec 5.3)")]
    PodcastScopes,
    #[error("a subscription row with replaced_by (spec 5.5)")]
    ReplacedBy,
}

impl ConformanceStore for MemoryStore {
    type Error = Unsupported;

    fn seed(&mut self, tables: &LocalTables) -> Result<(), Self::Error> {
        self.tables = tables.clone();
        Ok(())
    }

    fn apply_sync_response(&mut self, response: &SyncResponse) -> Result<(), Self::Error> {
        // Checked before any write, standing in for the rollback a real
        // store's transaction gives.
        if let Some(scopes) = &response.scopes {
            if !scopes.podcasts.is_empty() {
                return Err(Unsupported::PodcastScopes);
            }
            if scopes
                .user
                .subscriptions
                .iter()
                .any(|s| s.replaced_by.is_some())
            {
                return Err(Unsupported::ReplacedBy);
            }
        }
        // TODO: abort when the account row is missing or belongs to another
        // user (spec 5.3, 5.7).
        self.apply_results(response);
        if let Some(scopes) = &response.scopes {
            self.merge_user_scope(scopes);
            self.reconcile_podcast_scopes();
        }
        Ok(())
    }

    fn dump(&self) -> Result<LocalTables, Self::Error> {
        Ok(self.tables.clone())
    }
}

impl MemoryStore {
    /// Drops acknowledged outbox rows and copies a clamped `at` into the
    /// field the mutation wrote, if that field still carries the sent stamp.
    ///
    /// The stamp is compared with the mutation's own `dev`, not the current
    /// session's device id: a mutation queued before a re-login carries the
    /// old id, and so does the field it wrote.
    // TODO: on `rejected: invalid_feed_url`, delete the placeholder podcast,
    // its scope and its subscription row (spec 5.5).
    fn apply_results(&mut self, response: &SyncResponse) {
        for result in &response.results {
            let Some(index) = self
                .tables
                .outbox
                .iter()
                .position(|row| row.mutation_id == result.id)
            else {
                continue;
            };
            let sent = self.tables.outbox.remove(index).payload;
            let stored_at = match result.outcome {
                MutationOutcome::Applied { at } | MutationOutcome::Superseded { at } => at,
                MutationOutcome::Rejected { at: Some(at), .. } => at,
                MutationOutcome::Rejected { at: None, .. } => continue,
            };
            if stored_at != sent.at() {
                self.rewrite_stamp(&sent, stored_at);
            }
        }
    }

    fn rewrite_stamp(&mut self, sent: &Mutation, stored_at: Timestamp) {
        let sent_stamp = Stamp {
            at: sent.at(),
            dev: sent.dev(),
        };
        let carries_sent = |at: Option<Timestamp>, dev: Option<DeviceId>| {
            Stamp::from_columns(at, dev) == Some(sent_stamp)
        };
        match sent {
            Mutation::SubscriptionSet(m) => {
                if let Some(row) = self.subscription_mut(m.podcast_id)
                    && carries_sent(Some(row.subscribed_at), Some(row.subscribed_dev))
                {
                    row.subscribed_at = stored_at;
                }
            }
            Mutation::EpisodePosition(m) => {
                if let Some(row) = self.episode_state_mut(m.episode_id)
                    && carries_sent(row.position_at, row.position_dev)
                {
                    row.position_at = Some(stored_at);
                }
            }
            Mutation::EpisodePlayed(m) => {
                if let Some(row) = self.episode_state_mut(m.episode_id)
                    && carries_sent(row.played_at, row.played_dev)
                {
                    row.played_at = Some(stored_at);
                }
            }
            Mutation::EpisodeArchived(m) => {
                if let Some(row) = self.episode_state_mut(m.episode_id)
                    && carries_sent(row.archived_at, row.archived_dev)
                {
                    row.archived_at = Some(stored_at);
                }
            }
            Mutation::SettingSet(m) => {
                if let Some(row) = self.tables.setting.iter_mut().find(|row| row.key == m.key)
                    && carries_sent(Some(row.value_at), Some(row.value_dev))
                {
                    row.value_at = stored_at;
                }
            }
        }
    }

    fn merge_user_scope(&mut self, scopes: &ScopeChanges) {
        let user = &scopes.user;
        for remote in &user.subscriptions {
            match self.subscription_mut(remote.podcast_id) {
                Some(local) => local.merge(remote),
                None => self.tables.subscription.push(remote.into()),
            }
        }
        for remote in &user.episode_states {
            match self.episode_state_mut(remote.episode_id) {
                Some(local) => local.merge(remote),
                None => self.tables.episode_state.push(remote.into()),
            }
        }
        for remote in &user.settings {
            match self.tables.setting.iter_mut().find(|l| l.key == remote.key) {
                Some(local) => local.merge(remote),
                None => self.tables.setting.push(remote.into()),
            }
        }
        self.set_scope(SyncScopeRow {
            scope: ScopeKey::User,
            version: user.version,
            complete: user.complete,
        });
    }

    /// Every subscribed podcast has a scope row (new ones start at 0) and no
    /// unsubscribed one does, judged on the merged table (spec 5.3).
    // TODO: also delete an unsubscribed podcast's `podcast` and `episode`
    // rows once those tables are modeled (spec 5.3).
    fn reconcile_podcast_scopes(&mut self) {
        let subscribed: BTreeSet<ScopeKey> = self
            .tables
            .subscription
            .iter()
            .filter(|row| row.subscribed)
            .map(|row| ScopeKey::Podcast(row.podcast_id))
            .collect();
        self.tables
            .sync_scope
            .retain(|row| row.scope == ScopeKey::User || subscribed.contains(&row.scope));
        for scope in subscribed {
            if !self.tables.sync_scope.iter().any(|row| row.scope == scope) {
                self.tables.sync_scope.push(SyncScopeRow {
                    scope,
                    version: 0,
                    complete: false,
                });
            }
        }
    }

    fn set_scope(&mut self, new: SyncScopeRow) {
        match self
            .tables
            .sync_scope
            .iter_mut()
            .find(|r| r.scope == new.scope)
        {
            Some(row) => *row = new,
            None => self.tables.sync_scope.push(new),
        }
    }

    fn subscription_mut(&mut self, id: PodcastId) -> Option<&mut LocalSubscription> {
        self.tables
            .subscription
            .iter_mut()
            .find(|row| row.podcast_id == id)
    }

    fn episode_state_mut(&mut self, id: EpisodeId) -> Option<&mut LocalEpisodeState> {
        self.tables
            .episode_state
            .iter_mut()
            .find(|row| row.episode_id == id)
    }
}
