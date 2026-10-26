use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use pod_protocol::Timestamp;
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde_json::Value;

use super::plan::{Action, Conditions, plan};
use super::*;
use crate::store::tests::migrated_connection;

// ==============================================================================
// conformance/downloads.json
// ==============================================================================

/// Columns holding epoch ms, written as RFC 3339 in the fixtures.
const TIME_COLUMNS: &[&str] = &[
    "published_at",
    "played_at",
    "archived_at",
    "position_at",
    "created_at",
    "next_attempt_at",
    "completed_at",
    "unwanted_since",
];
const FLAG_COLUMNS: &[&str] = &[
    "played",
    "archived",
    "metered_ok",
    "auto_on_metered",
    "notify",
];

/// Columns the reconciler never reads, filled in when a fixture row leaves
/// them out.
fn filler(table: &str) -> &'static [(&'static str, &'static str)] {
    match table {
        "episode" => &[
            ("title", "'An episode'"),
            ("summary_html", "''"),
            ("content_html", "''"),
            ("audio_type", "'audio/mpeg'"),
            ("version", "1"),
        ],
        "download" => &[("path", "''")],
        _ => &[],
    }
}

fn ms(text: &str) -> i64 {
    text.parse::<Timestamp>()
        .unwrap_or_else(|error| panic!("{text:?} is RFC 3339: {error}"))
        .as_epoch_ms()
}

fn to_sql(table: &str, column: &str, value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::String(text) if TIME_COLUMNS.contains(&column) => SqlValue::Integer(ms(text)),
        Value::Bool(flag) if FLAG_COLUMNS.contains(&column) => SqlValue::Integer(i64::from(*flag)),
        _ if table == "device_setting" && column == "value" => SqlValue::Text(value.to_string()),
        Value::String(text) => SqlValue::Text(text.clone()),
        Value::Number(number) => SqlValue::Integer(
            number
                .as_i64()
                .unwrap_or_else(|| panic!("{table}.{column} = {number} is an integer")),
        ),
        _ => panic!("{table}.{column} = {value} has no SQL form"),
    }
}

fn to_json(column: &str, value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(ms) if TIME_COLUMNS.contains(&column) => Value::String(
            Timestamp::from_epoch_ms(ms)
                .expect("stored timestamps are in range")
                .to_string(),
        ),
        ValueRef::Integer(flag) if FLAG_COLUMNS.contains(&column) => Value::Bool(flag != 0),
        ValueRef::Integer(number) => Value::from(number),
        ValueRef::Text(text) => Value::String(String::from_utf8_lossy(text).into_owned()),
        other => panic!("{column} holds {other:?}"),
    }
}

fn seed(connection: &Connection, tables: &Value) {
    for (table, rows) in tables.as_object().expect("`before` is an object") {
        for row in rows.as_array().expect("a table is an array of rows") {
            let row = row.as_object().expect("a row is an object");
            let mut columns: Vec<String> = Vec::new();
            let mut placeholders: Vec<String> = Vec::new();
            let mut values: Vec<SqlValue> = Vec::new();
            for (column, value) in row {
                columns.push(column.clone());
                placeholders.push(format!("?{}", values.len() + 1));
                values.push(to_sql(table, column, value));
            }
            for (column, literal) in filler(table) {
                if !row.contains_key(*column) {
                    columns.push((*column).to_owned());
                    placeholders.push((*literal).to_owned());
                }
            }
            connection
                .execute(
                    &format!(
                        "insert into {table} ({}) values ({})",
                        columns.join(", "),
                        placeholders.join(", ")
                    ),
                    rusqlite::params_from_iter(values),
                )
                .unwrap_or_else(|error| panic!("seeding {table} with {row:?}: {error}"));
        }
    }
}

/// Checks `expected` against the store: every named table's rows match one
/// to one by primary key, and only the listed columns are compared.
fn check(connection: &Connection, expected: &Value, case: &str) {
    for (table, rows) in expected.as_object().expect("`after` is an object") {
        let key: &[&str] = match table.as_str() {
            "download" => &["episode_id"],
            "download_wish" => &["episode_id", "source"],
            other => panic!("{case}: `after` can't check {other}"),
        };
        let key_of = |row: &serde_json::Map<String, Value>| -> Vec<Value> {
            key.iter()
                .map(|column| {
                    row.get(*column)
                        .unwrap_or_else(|| panic!("{case}: an expected {table} row names {column}"))
                        .clone()
                })
                .collect()
        };
        let mut statement = connection
            .prepare(&format!("select * from {table}"))
            .expect("the table exists");
        let names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let actual: Vec<serde_json::Map<String, Value>> = statement
            .query_map([], |row| {
                Ok(names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        (
                            name.clone(),
                            to_json(name, row.get_ref(index).expect("the column exists")),
                        )
                    })
                    .collect())
            })
            .expect("the table reads")
            .collect::<rusqlite::Result<_>>()
            .expect("the rows read");
        let expected_rows = rows.as_array().expect("a table is an array of rows");
        let mut actual_keys: Vec<Vec<Value>> = actual.iter().map(key_of).collect();
        let mut expected_keys: Vec<Vec<Value>> = expected_rows
            .iter()
            .map(|row| key_of(row.as_object().expect("a row is an object")))
            .collect();
        actual_keys.sort_by_key(|key| format!("{key:?}"));
        expected_keys.sort_by_key(|key| format!("{key:?}"));
        assert_eq!(actual_keys, expected_keys, "{case}: the {table} rows");
        for expected_row in expected_rows {
            let expected_row = expected_row.as_object().expect("a row is an object");
            let actual_row = actual
                .iter()
                .find(|row| key_of(row) == key_of(expected_row))
                .expect("the keys matched");
            for (column, value) in expected_row {
                let actual_value = actual_row
                    .get(column)
                    .unwrap_or_else(|| panic!("{case}: {table} has no column {column}"));
                assert_eq!(
                    actual_value,
                    value,
                    "{case}: {table}.{column} of {:?}",
                    key_of(expected_row)
                );
            }
        }
    }
}

fn conditions_of(case: &Value) -> Conditions {
    let conditions = &case["conditions"];
    let flag = |key: &str| {
        conditions[key]
            .as_bool()
            .unwrap_or_else(|| panic!("conditions.{key} is a boolean"))
    };
    Conditions {
        now: ms(conditions["now"]
            .as_str()
            .expect("conditions.now is a string")),
        network: NetworkState {
            connected: flag("connected"),
            metered: flag("metered"),
        },
        free_bytes: conditions["free_bytes"]
            .as_u64()
            .expect("conditions.free_bytes is a number"),
        playing: conditions["playing"].as_str().map(str::to_owned),
        live: conditions["live"]
            .as_array()
            .expect("conditions.live is an array")
            .iter()
            .map(|id| id.as_str().expect("live ids are strings").to_owned())
            .collect(),
    }
}

fn ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("an id list is an array")
        .iter()
        .map(|id| id.as_str().expect("ids are strings").to_owned())
        .collect()
}

/// One pass on `connection`: load, plan, apply. Returns the plan and what
/// applying it left for the executor.
fn pass(connection: &mut Connection, conditions: &Conditions) -> (plan::Plan, Applied) {
    let snapshot = load(connection).expect("the tables load");
    let plan = plan(&snapshot, conditions);
    let transaction = connection.transaction().expect("a transaction begins");
    let mut runs = 0;
    let applied = apply(&transaction, &plan.actions, || {
        runs += 1;
        format!("0199b000-0000-7000-8000-{runs:012}")
    })
    .expect("the plan applies");
    transaction.commit().expect("the plan commits");
    (plan, applied)
}

/// Runs `conformance/downloads.json`; the README next to it describes the
/// format.
#[test]
fn every_download_conformance_case_passes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/downloads.json");
    let text = std::fs::read_to_string(&path).expect("conformance/downloads.json is readable");
    let fixture: Value = serde_json::from_str(&text).expect("the fixture is JSON");
    for case in fixture["cases"].as_array().expect("`cases` is an array") {
        let name = case["name"].as_str().expect("a case has a name");
        let mut connection = migrated_connection();
        seed(&connection, &case["before"]);
        let mut conditions = conditions_of(case);

        let (first, applied) = pass(&mut connection, &conditions);
        check(&connection, &case["after"], name);
        let started: Vec<String> = applied
            .starts
            .iter()
            .map(|(episode_id, _, _)| episode_id.clone())
            .collect();
        assert_eq!(started, ids(&case["started"]), "{name}: started");
        let skipped: BTreeSet<String> = ids(&case["skipped"]).into_iter().collect();
        assert_eq!(first.skipped, skipped, "{name}: skipped");

        // Between the passes every cancelled job has ended and every
        // started one is running.
        let cancelled: HashSet<&String> = applied.cancels.iter().collect();
        conditions
            .live
            .retain(|episode_id| !cancelled.contains(episode_id));
        conditions.live.extend(started);
        let (second, _) = pass(&mut connection, &conditions);
        assert_eq!(
            second.actions,
            Vec::<Action>::new(),
            "{name}: the second pass changes nothing"
        );
        assert_eq!(second.skipped, first.skipped, "{name}: skipped stays");
    }
}

// ==============================================================================
// The executor against a live server
// ==============================================================================

mod live {
    use std::time::{Duration, Instant};

    use pod_protocol::{EpisodeId, PodcastId};
    use tempfile::TempDir;
    use tokio::sync::watch;

    use super::super::*;
    use crate::download::tests::{BODY_LEN, ensure_server, ranges_seen};
    use crate::download::{Downloader, client_builder};
    use crate::store::tests::scratch_dir;
    use crate::sync::SyncHandle;
    use crate::sync::local_write::{self, EpisodeChange, LocalChange};

    const PODCAST: &str = "a1000000-0000-5000-8000-000000000001";
    const DEVICE: &str = "0199c000-0000-7000-8000-000000000001";

    fn episode(n: u32) -> String {
        format!("e0000000-0000-5000-8000-{n:012}")
    }

    fn plenty_of_space(_: &Path) -> Result<u64> {
        Ok(1 << 40)
    }

    struct Fixture {
        dir: TempDir,
        store: Store,
        reconciler: Reconciler,
        sync: SyncEngine,
        sync_handle: SyncHandle,
        _network: watch::Sender<NetworkState>,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = scratch_dir();
            let store = Store::open(&dir.path().join("pod.db")).expect("the store opens");
            let (network, network_rx) = watch::channel(NetworkState::ASSUMED);
            let downloader = Downloader {
                store: store.clone(),
                // The sandbox routes outbound HTTP through a proxy; loopback
                // must not.
                client: client_builder()
                    .no_proxy()
                    .build()
                    .expect("the test client builds"),
                root: dir.path().join("audio"),
                network: network_rx.clone(),
                checkpoint_every: Duration::ZERO,
            };
            let sync_handle = SyncHandle::default();
            let sync = SyncEngine::new(store.clone(), sync_handle.clone(), network_rx)
                .expect("the sync engine builds");
            Fixture {
                reconciler: Reconciler::new(downloader, plenty_of_space),
                dir,
                store,
                sync,
                sync_handle,
                _network: network,
            }
        }

        fn root(&self) -> PathBuf {
            self.dir.path().join("audio")
        }

        async fn execute(&self, sql: &'static str, values: Vec<rusqlite::types::Value>) {
            self.store
                .write(move |transaction| {
                    transaction
                        .execute(sql, rusqlite::params_from_iter(values))
                        .context("run the seed statement")
                })
                .await
                .expect("the seed commits");
        }

        async fn insert_episode(&self, n: u32, url: String) {
            self.execute(
                "insert into episode (id, podcast_id, title, summary_html, content_html,
                     published_at, audio_url, audio_type, version)
                 values (?1, ?2, 'An episode', '', '', ?3, ?4, 'audio/mpeg', 1)",
                vec![
                    episode(n).into(),
                    PODCAST.to_owned().into(),
                    i64::from(n).into(),
                    url.into(),
                ],
            )
            .await;
        }

        /// `(state, path, unwanted_since)` of each row, by episode id.
        async fn rows(&self) -> HashMap<String, (String, String, Option<i64>)> {
            self.store
                .read(|connection| {
                    connection
                        .prepare("select episode_id, state, path, unwanted_since from download")
                        .context("prepare")?
                        .query_map([], |row| {
                            Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
                        })
                        .context("query")?
                        .collect::<rusqlite::Result<_>>()
                        .context("read")
                })
                .await
                .expect("the rows read")
        }

        async fn wait_until(
            &self,
            what: &str,
            done: impl Fn(&HashMap<String, (String, String, Option<i64>)>) -> bool,
        ) -> HashMap<String, (String, String, Option<i64>)> {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let rows = self.rows().await;
                if done(&rows) {
                    return rows;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting until {what}: {rows:?}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    fn complete(rows: &HashMap<String, (String, String, Option<i64>)>, n: u32) -> bool {
        rows.get(&episode(n)).is_some_and(|row| row.0 == "complete")
    }

    /// The issue's acceptance: "keep newest 3" downloads 3, and marking one
    /// played swaps in the next, with the played file deleted after the
    /// grace period. Playing it holds the file past the grace period.
    #[tokio::test]
    async fn keep_newest_three_swaps_in_the_next_after_the_grace_period() {
        let server = ensure_server();
        let fixture = Fixture::new();
        fixture
            .execute(
                "insert into account (id, server_url, user_id, username, device_id, token)
                 values (1, 'http://127.0.0.1:1', '0199c000-0000-7000-8000-000000000002',
                     'alice', ?1, 'token')",
                vec![DEVICE.to_owned().into()],
            )
            .await;
        fixture
            .execute(
                "insert into podcast_device (podcast_id, auto_keep) values (?1, 3)",
                vec![PODCAST.to_owned().into()],
            )
            .await;
        // 1.8 s.
        fixture
            .execute(
                "insert into device_setting (key, value) values ('evict_grace_hours', '0.0005')",
                vec![],
            )
            .await;
        for n in 1..=4 {
            fixture
                .insert_episode(n, format!("{server}/audio/reconcile-{n}"))
                .await;
        }
        let run = tokio::spawn(fixture.reconciler.clone().run(fixture.sync.clone()));

        let rows = fixture
            .wait_until("the newest three are complete", |rows| {
                (2..=4).all(|n| complete(rows, n))
            })
            .await;
        assert!(
            !rows.contains_key(&episode(1)),
            "only three download: {rows:?}"
        );
        for n in 2..=4 {
            let path = fixture.root().join(&rows[&episode(n)].1);
            let size = std::fs::metadata(&path).expect("the file exists").len();
            assert_eq!(size, BODY_LEN as u64);
        }
        assert!(fixture.reconciler.skipped().borrow().is_empty());

        let newest: EpisodeId = episode(4).parse().expect("a UUID");
        fixture.reconciler.set_playing(Some(newest));
        local_write::write(
            &fixture.store,
            &fixture.sync_handle,
            LocalChange::Episode {
                episode_id: newest,
                podcast_id: PODCAST.parse::<PodcastId>().expect("a UUID"),
                change: EpisodeChange::Played(true),
            },
        )
        .await
        .expect("the played write commits");

        let rows = fixture
            .wait_until("the next one is complete", |rows| complete(rows, 1))
            .await;
        let played_path = fixture.root().join(&rows[&episode(4)].1);
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let rows = fixture.rows().await;
        assert_eq!(
            rows[&episode(4)].2,
            None,
            "the playing file is wanted past the grace period"
        );

        fixture.reconciler.set_playing(None);
        fixture
            .wait_until("the played file starts its grace period", |rows| {
                rows.get(&episode(4)).is_some_and(|row| row.2.is_some())
            })
            .await;
        assert!(played_path.exists(), "kept during the grace period");
        let rows = fixture
            .wait_until("the played file is evicted", |rows| {
                !rows.contains_key(&episode(4))
            })
            .await;
        assert!(!played_path.exists());
        assert!((1..=3).all(|n| complete(&rows, n)), "{rows:?}");
        run.abort();
    }

    #[tokio::test]
    async fn cancel_all_stops_transfers_and_holds_off_passes() {
        let server = ensure_server();
        let fixture = Fixture::new();
        fixture
            .insert_episode(1, format!("{server}/audio/reconcile-cancel?trickle"))
            .await;
        fixture
            .execute(
                "insert into download_wish (episode_id, source, created_at) values (?1, 'pin', 0)",
                vec![episode(1).into()],
            )
            .await;
        let run = tokio::spawn(fixture.reconciler.clone().run(fixture.sync.clone()));
        fixture
            .wait_until("the transfer runs", |rows| {
                rows.get(&episode(1))
                    .is_some_and(|row| row.0 == "transferring")
            })
            .await;

        let stopped = fixture.reconciler.cancel_all(Duration::from_secs(10)).await;
        assert!(
            fixture.reconciler.inner.jobs.try_lock().is_err(),
            "passes are held off"
        );
        // A trigger while stopped starts nothing.
        fixture
            .execute(
                "update download_wish set metered_ok = 1 where episode_id = ?1",
                vec![episode(1).into()],
            )
            .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(fixture.rows().await[&episode(1)].0, "transferring");
        drop(stopped);

        // Without a wipe, the next pass requeues the orphan and starts it.
        fixture
            .wait_until("the transfer completes after all", |rows| complete(rows, 1))
            .await;
        run.abort();
    }

    #[tokio::test]
    async fn logout_stops_the_transfer_and_leaves_no_audio_behind() {
        let server = ensure_server();
        let fixture = Fixture::new();
        fixture
            .insert_episode(1, format!("{server}/audio/reconcile-logout?trickle"))
            .await;
        fixture
            .execute(
                "insert into download_wish (episode_id, source, created_at) values (?1, 'pin', 0)",
                vec![episode(1).into()],
            )
            .await;
        let run = tokio::spawn(fixture.reconciler.clone().run(fixture.sync.clone()));
        fixture
            .wait_until("the transfer runs", |rows| {
                rows.get(&episode(1))
                    .is_some_and(|row| row.0 == "transferring")
            })
            .await;
        assert!(fixture.root().exists());

        let session = crate::sync::session::Session::new(
            fixture.store.clone(),
            fixture.sync_handle.clone(),
            fixture.sync.clone(),
            fixture.reconciler.clone(),
        );
        session.logout().await.expect("logout works");

        assert!(fixture.rows().await.is_empty());
        assert!(!fixture.root().exists());
        assert_eq!(
            ranges_seen("reconcile-logout").len(),
            1,
            "no pass restarted the transfer during logout"
        );
        // The wipe's table events wake the loop once the pass lock is free;
        // that pass finds nothing to download.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            fixture
                .reconciler
                .inner
                .jobs
                .try_lock()
                .expect("no pass is running")
                .is_empty(),
            "no transfer runs"
        );
        // A pass recreates the empty root for its free-space check.
        let files: Vec<_> = walk(&fixture.root());
        assert!(files.is_empty(), "no audio file comes back: {files:?}");
        run.abort();
    }

    /// Every file under `root`, recursively.
    fn walk(root: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .flat_map(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn startup_repair_publishes_requeues_drops_and_sweeps() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let directory = root.join(PODCAST);
        std::fs::create_dir_all(&directory).expect("the podcast directory is creatable");
        let run = |n: u32| format!("0199a000-0000-7000-8000-{n:012}");
        let file =
            |n: u32, suffix: &str| directory.join(format!("{}.{}.mp3{suffix}", episode(n), run(n)));
        for n in 1..=4 {
            fixture
                .insert_episode(n, format!("https://example.com/e{n}.mp3"))
                .await;
        }
        let seed = |n: u32, state: &str, path: String| {
            vec![
                episode(n).into(),
                state.to_owned().into(),
                path.into(),
                run(n).into(),
            ]
        };
        let insert =
            "insert into download (episode_id, state, audio_url, path, bytes_total, run_id)
                      values (?1, ?2, 'https://example.com/a.mp3', ?3, 10, ?4)";
        // 1: crashed between rename and row write.
        std::fs::write(file(1, ""), [0; 10]).expect("writable");
        fixture
            .execute(insert, seed(1, "transferring", String::new()))
            .await;
        // 2: crashed mid-transfer.
        std::fs::write(file(2, ".part"), [0; 4]).expect("writable");
        fixture
            .execute(insert, seed(2, "transferring", String::new()))
            .await;
        // 3: complete, but the file is gone.
        fixture
            .execute(
                insert,
                seed(
                    3,
                    "complete",
                    format!("{PODCAST}/{}.{}.mp3", episode(3), run(3)),
                ),
            )
            .await;
        // 4: complete and present.
        std::fs::write(file(4, ""), [0; 10]).expect("writable");
        fixture
            .execute(
                insert,
                seed(
                    4,
                    "complete",
                    format!("{PODCAST}/{}.{}.mp3", episode(4), run(4)),
                ),
            )
            .await;
        // Leftovers nothing accounts for.
        std::fs::write(file(9, ""), [0; 10]).expect("writable");
        std::fs::write(file(9, ".part"), [0; 10]).expect("writable");
        std::fs::write(root.join("stray"), [0; 1]).expect("writable");

        fixture.reconciler.repair().await.expect("the repair runs");

        let rows = fixture.rows().await;
        assert_eq!(rows[&episode(1)].0, "complete");
        assert_eq!(
            rows[&episode(1)].1,
            format!("{PODCAST}/{}.{}.mp3", episode(1), run(1))
        );
        assert_eq!(
            rows[&episode(2)].0,
            "transferring",
            "the first pass requeues it"
        );
        assert!(!rows.contains_key(&episode(3)));
        assert_eq!(rows[&episode(4)].0, "complete");
        assert!(file(1, "").exists() && file(2, ".part").exists() && file(4, "").exists());
        assert!(
            !file(9, "").exists() && !file(9, ".part").exists() && !root.join("stray").exists()
        );
    }
}
