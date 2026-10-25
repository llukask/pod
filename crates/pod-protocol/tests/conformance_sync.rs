//! Runs the state-transition fixtures (`conformance/sync_*.json`) against
//! the reference in-memory store, and checks every fixture file has a runner.

use std::path::{Path, PathBuf};

use pod_protocol::conformance::{MemoryStore, run_sync_fixtures, sync_fixture_paths};

fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

#[test]
fn every_sync_fixture_passes_against_the_memory_store() {
    let failures =
        run_sync_fixtures(&conformance_dir(), MemoryStore::default).expect("fixtures are readable");
    let report: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert!(report.is_empty(), "\n{}", report.join("\n"));
}

#[test]
fn the_sync_fixtures_are_found() {
    let paths = sync_fixture_paths(&conformance_dir()).expect("the directory is readable");
    assert!(paths.len() >= 2, "found only {paths:?}");
}

/// The file name picks the runner (see `conformance/README.md`), so a file
/// no runner claims would be skipped without a trace.
#[test]
fn every_fixture_file_has_a_runner() {
    for entry in std::fs::read_dir(conformance_dir()).expect("the directory is readable") {
        let path = entry.expect("entries are readable").path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("fixture names are UTF-8");
        // `inbox.json` needs a database, so pod-server's
        // `tests/conformance_inbox.rs` runs it.
        let claimed =
            matches!(name, "ids.json" | "lww.json" | "inbox.json") || name.starts_with("sync_");
        assert!(claimed, "no runner reads conformance/{name}");
    }
}
