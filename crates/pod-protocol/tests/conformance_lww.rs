//! Runs `conformance/lww.json`, the rule tables the server and both clients
//! also run.

use std::collections::BTreeMap;
use std::path::Path;

use pod_protocol::{DeviceId, Stamp, Timestamp, clamp_mutation_at, wins};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    #[expect(dead_code, reason = "documentation for fixture readers")]
    description: String,
    devices: BTreeMap<DeviceId, String>,
    wins: Vec<WinsCase>,
    clamp: Vec<ClampCase>,
    apply: Vec<ApplyCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WinsCase {
    name: String,
    at: Timestamp,
    dev: DeviceId,
    old_at: Option<Timestamp>,
    old_dev: Option<DeviceId>,
    wins: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClampCase {
    name: String,
    at: Timestamp,
    server_now: Timestamp,
    stored_at: Timestamp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyCase {
    name: String,
    server_now: Timestamp,
    before: Option<Field>,
    mutations: Vec<Field>,
    results: Vec<ApplyResult>,
    after: Field,
}

/// One field's value and stamp: the stored field, or what a mutation writes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Field {
    value: serde_json::Value,
    at: Timestamp,
    dev: DeviceId,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyResult {
    status: String,
    at: Timestamp,
}

fn fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/lww.json");
    let text = std::fs::read_to_string(&path).expect("conformance/lww.json is checked in");
    serde_json::from_str(&text).expect("conformance/lww.json matches the fixture schema")
}

fn stamp(field: &Field) -> Stamp {
    Stamp {
        at: field.at,
        dev: field.dev,
    }
}

#[test]
fn wins_decides_every_case() {
    let mut failures = Vec::new();
    for case in fixture().wins {
        let new = Stamp {
            at: case.at,
            dev: case.dev,
        };
        let old = Stamp::from_columns(case.old_at, case.old_dev);
        if wins(new, old) != case.wins {
            failures.push(format!("{}: expected wins = {}", case.name, case.wins));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn clamp_stores_the_expected_at() {
    let mut failures = Vec::new();
    for case in fixture().clamp {
        let stored = clamp_mutation_at(case.at, case.server_now);
        if stored != case.stored_at {
            failures.push(format!(
                "{}: expected {}, got {stored}",
                case.name, case.stored_at
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Spec 4.4's `apply` for one field, without the database: clamp each
/// mutation's `at`, then write it if it wins against the stored stamp.
#[test]
fn applying_mutations_in_order_ends_in_the_expected_field() {
    let mut failures = Vec::new();
    for case in fixture().apply {
        let mut field = case.before.clone();
        let mut results = Vec::new();
        for mutation in &case.mutations {
            let at = clamp_mutation_at(mutation.at, case.server_now);
            let clamped = Field {
                at,
                ..mutation.clone()
            };
            let status = if wins(stamp(&clamped), field.as_ref().map(stamp)) {
                field = Some(clamped);
                "applied"
            } else {
                "superseded"
            };
            results.push(ApplyResult {
                status: status.into(),
                at,
            });
        }
        if results != case.results {
            failures.push(format!(
                "{}: expected results {:?}, got {results:?}",
                case.name, case.results
            ));
        }
        if field.as_ref() != Some(&case.after) {
            failures.push(format!(
                "{}: expected field {:?}, got {field:?}",
                case.name, case.after
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_device_id_is_documented() {
    let fixture = fixture();
    let used = fixture
        .wins
        .iter()
        .flat_map(|case| [Some(case.dev), case.old_dev])
        .chain(fixture.apply.iter().flat_map(|case| {
            case.before
                .iter()
                .chain(&case.mutations)
                .chain([&case.after])
                .map(|field| Some(field.dev))
                .collect::<Vec<_>>()
        }))
        .flatten();
    for dev in used {
        assert!(
            fixture.devices.contains_key(&dev),
            "{dev} is missing from `devices`"
        );
    }
}
