//! Runs the client state-transition fixtures (`conformance/sync_*.json` and
//! `conformance/client_*.json`, spec section 9) against the SQLite store.
//!
//! Each case seeds a scratch store with `local_before`, runs its steps
//! through the same functions the engine and the UI call, and compares the
//! tables with `local_after`. The server is the fixture: a `send` step builds
//! the request the engine would post and marks its batch in flight, and the
//! following `receive` step applies the fixture's response in its place.
//! `conformance/README.md` documents the format.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{OptionExt as _, Result, WrapErr, bail};
use pod_protocol::conformance::compare_tables;
use pod_protocol::{Mutation, SyncResponse, Timestamp, UserId};
use rusqlite::{Connection, OptionalExtension as _};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::SyncHandle;
use super::engine::{Round, finish_round, prepare};
use super::local_write::{self, EpisodeChange, LocalChange};
use super::session::delete_account_data;
use crate::store::json_rows;
use crate::store::tests::migrated_connection;

// ==============================================================================
// Fixture format
// ==============================================================================

type Tables = BTreeMap<String, Vec<Map<String, Value>>>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureFile {
    #[expect(dead_code, reason = "documentation for the reader of the file")]
    description: String,
    cases: Vec<Case>,
}

/// One case. It has either `server_response`, short for a `send` followed by
/// a `receive` of that response, or `steps`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    local_before: Tables,
    #[serde(default)]
    server_response: Option<SyncResponse>,
    #[serde(default)]
    steps: Option<Vec<Step>>,
    local_after: Tables,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    /// The engine selects its batch, marks it in flight and builds the
    /// request. `request` lists top-level request keys to compare exactly.
    Send {
        #[serde(default)]
        request: Option<Map<String, Value>>,
    },
    /// The response to the last `send` arrives and is applied. `loop` is
    /// whether the sync loop then sends again.
    Receive {
        response: SyncResponse,
        #[serde(default, rename = "loop")]
        then: Option<LoopOutcome>,
    },
    /// A user action at clock `now`: the wire mutation it queues, without
    /// `at` and `dev`, which the write computes.
    Write {
        now: Timestamp,
        mutation: Map<String, Value>,
    },
    /// Logout's table wipe (spec 5.7).
    Logout,
    /// A partial expectation in the middle of the case, like `local_after`.
    Expect { tables: Tables },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LoopOutcome {
    Continue,
    Stop,
}

// ==============================================================================
// Running a case
// ==============================================================================

fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

/// The fixture files this runner claims, sorted by name.
fn fixture_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(conformance_dir())
        .expect("conformance/ is readable")
        .map(|entry| entry.expect("entries are readable").path())
        .filter(|path| {
            let name = path.file_name().and_then(|name| name.to_str());
            name.is_some_and(|name| {
                (name.starts_with("sync_") || name.starts_with("client_"))
                    && name.ends_with(".json")
            })
        })
        .collect();
    paths.sort();
    paths
}

/// Runs `case` on a fresh store. `Err` lists every difference; a step that
/// fails outright ends the case.
fn run_case(case: &Case) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    if let Err(error) = run_steps(case, &mut problems) {
        problems.push(format!("{error:#}"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn run_steps(case: &Case, problems: &mut Vec<String>) -> Result<()> {
    let mut connection = migrated_connection();
    for (table, rows) in &case.local_before {
        for row in rows {
            json_rows::insert(&connection, table, row).context("seed local_before")?;
        }
    }

    let shorthand;
    let steps = match (&case.server_response, &case.steps) {
        (Some(response), None) => {
            shorthand = [
                Step::Send { request: None },
                Step::Receive {
                    response: response.clone(),
                    then: None,
                },
            ];
            &shorthand[..]
        }
        (None, Some(steps)) => &steps[..],
        _ => bail!("a case has either `server_response` or `steps`"),
    };

    let handle = SyncHandle::default();
    // The user the last request was made for, until its response arrives.
    let mut sent: Option<UserId> = None;
    for (index, step) in steps.iter().enumerate() {
        let at = format!("step {}", index + 1);
        let transaction = connection
            .transaction()
            .context("begin the step's transaction")?;
        match step {
            Step::Send { request } => {
                let prepared = prepare(&transaction, false, &handle)
                    .with_context(|| format!("{at}: prepare the request"))?
                    .ok_or_eyre(format!("{at}: no account to send for"))?;
                let actual =
                    serde_json::to_value(&prepared.request).context("serialize the request")?;
                for (key, want) in request.iter().flatten() {
                    let got = actual.get(key).unwrap_or(&Value::Null);
                    if got != want {
                        problems.push(format!("{at}: request.{key}: want {want}, got {got}"));
                    }
                }
                sent = Some(prepared.user_id);
            }
            Step::Receive { response, then } => {
                let user_id = sent
                    .take()
                    .ok_or_eyre(format!("{at}: a receive needs a send before it"))?;
                let round = finish_round(&transaction, &handle, user_id, response, false)
                    .with_context(|| format!("{at}: apply the response"))?;
                let outcome = match round {
                    Round::More => LoopOutcome::Continue,
                    _ => LoopOutcome::Stop,
                };
                if let Some(want) = then
                    && *want != outcome
                {
                    problems.push(format!("{at}: loop: want {want:?}, got {outcome:?}"));
                }
            }
            Step::Write { now, mutation } => {
                let (id, change) = local_change(&transaction, mutation, *now)
                    .with_context(|| format!("{at}: read the write"))?;
                let in_flight = handle.lock_in_flight();
                local_write::apply(&transaction, change, id, &in_flight, *now)
                    .with_context(|| format!("{at}: write locally"))?;
            }
            Step::Logout => {
                delete_account_data(&transaction).with_context(|| format!("{at}: log out"))?;
            }
            Step::Expect { tables } => {
                let actual = json_rows::dump(&transaction).context("dump the tables")?;
                if let Err(differences) = compare_tables(&actual, tables) {
                    problems.extend(differences.into_iter().map(|d| format!("{at}: {d}")));
                }
            }
        }
        transaction
            .commit()
            .with_context(|| format!("{at}: commit"))?;
    }

    let actual = json_rows::dump(&connection).context("dump the tables")?;
    if let Err(differences) = compare_tables(&actual, &case.local_after) {
        problems.extend(differences);
    }
    Ok(())
}

/// The user action behind a `write` step's mutation. An episode action takes
/// its podcast id from the episode's state row or, failing that, its
/// episode row; the wire mutation doesn't carry it.
fn local_change(
    connection: &Connection,
    mutation: &Map<String, Value>,
    now: Timestamp,
) -> Result<(pod_protocol::MutationId, LocalChange)> {
    // `at` and `dev` are placeholders for parsing; the write stamps its own.
    let mut stamped = mutation.clone();
    stamped.insert(
        "at".into(),
        serde_json::to_value(now).context("encode now")?,
    );
    stamped.insert(
        "dev".into(),
        Value::from("00000000-0000-0000-0000-000000000000"),
    );
    let mutation: Mutation =
        serde_json::from_value(Value::Object(stamped)).context("parse the mutation")?;
    let podcast_of = |episode_id: pod_protocol::EpisodeId| -> Result<pod_protocol::PodcastId> {
        let id: String = connection
            .query_row(
                "select podcast_id from episode_state where episode_id = ?1
                 union all select podcast_id from episode where id = ?1",
                [episode_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .context("look up the episode's podcast")?
            .ok_or_eyre("the episode has no state or episode row")?;
        id.parse().context("parse the podcast id")
    };
    let episode = |episode_id, change| -> Result<LocalChange> {
        Ok(LocalChange::Episode {
            episode_id,
            podcast_id: podcast_of(episode_id).context("find the episode's podcast")?,
            change,
        })
    };
    let change = match &mutation {
        // Playback's periodic saves only differ in how soon they sync.
        Mutation::EpisodePosition(m) => episode(
            m.episode_id,
            EpisodeChange::Position {
                position_ms: m.position_ms,
                while_playing: false,
            },
        )?,
        Mutation::EpisodePlayed(m) => episode(m.episode_id, EpisodeChange::Played(m.played))?,
        Mutation::EpisodeArchived(m) => episode(m.episode_id, EpisodeChange::Archived(m.archived))?,
        Mutation::SubscriptionSet(m) => LocalChange::Subscription {
            podcast_id: m.podcast_id,
            feed_url: m.feed_url.clone(),
            subscribed: m.subscribed,
        },
        Mutation::SettingSet(m) => LocalChange::Setting {
            key: m.key.clone(),
            value: m.value.clone(),
        },
    };
    Ok((mutation.id(), change))
}

#[test]
fn every_client_sync_conformance_case_passes() {
    let paths = fixture_paths();
    assert!(
        paths.iter().any(|path| path.ends_with("client_sync.json")),
        "found only {paths:?}"
    );
    let mut report = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path).expect("fixture files are readable");
        let file: FixtureFile = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{} doesn't parse: {error}", path.display()));
        for case in &file.cases {
            if let Err(problems) = run_case(case) {
                report.push(format!("{}: {}", path.display(), case.name));
                report.extend(problems.into_iter().map(|problem| format!("    {problem}")));
            }
        }
    }
    assert!(report.is_empty(), "\n{}", report.join("\n"));
}
