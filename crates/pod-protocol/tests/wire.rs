//! Wire shapes of the entities, auth bodies and mutations.

mod common;

use common::*;
use pod_protocol::*;
use serde_json::json;

// ==============================================================================
// Entities
// ==============================================================================

#[test]
fn podcast_wire_shape() {
    insta::assert_json_snapshot!(atp_podcast());
    assert_round_trips(&atp_podcast());
}

#[test]
fn podcast_statuses_are_snake_case() {
    let statuses = [
        PodcastStatus::Pending,
        PodcastStatus::Ok,
        PodcastStatus::Failing,
        PodcastStatus::Gone,
    ];
    insta::assert_json_snapshot!(statuses, @r#"
    [
      "pending",
      "ok",
      "failing",
      "gone"
    ]
    "#);
    for status in statuses {
        assert_round_trips(&status);
    }
}

#[test]
fn live_episode_carries_deleted_false() {
    let episode = ScopeEpisode::Live(atp_episode());
    insta::assert_json_snapshot!(episode);
    assert_round_trips(&episode);
}

#[test]
fn tombstone_carries_only_id_deleted_and_version() {
    let tombstone = ScopeEpisode::Deleted(EpisodeTombstone {
        id: EPISODE_ATP_611,
        version: 900,
    });
    insta::assert_json_snapshot!(tombstone, @r#"
    {
      "id": "5c2f8b3d-4e6a-5f7b-9cad-1e2f3a4b5c6d",
      "version": 900,
      "deleted": true
    }
    "#);
    assert_round_trips(&tombstone);
}

#[test]
fn scope_episode_without_a_boolean_deleted_is_rejected() {
    for deleted in [None, Some(json!("false"))] {
        let mut json =
            serde_json::to_value(ScopeEpisode::Live(atp_episode())).expect("episodes serialize");
        let object = json.as_object_mut().expect("episodes are objects");
        object.remove("deleted");
        if let Some(deleted) = deleted {
            object.insert("deleted".into(), deleted);
        }
        let error = serde_json::from_value::<ScopeEpisode>(json).expect_err("deleted is required");
        assert!(error.to_string().contains("deleted"), "{error}");
    }
}

#[test]
fn subscription_wire_shape() {
    insta::assert_json_snapshot!(atp_subscription());
    assert_round_trips(&atp_subscription());
}

#[test]
fn replaced_subscription_wire_shape() {
    let replaced = Subscription {
        subscribed: false,
        replaced_by: Some(PODCAST_HARDCORE_HISTORY),
        version: 1844,
        ..atp_subscription()
    };
    assert_round_trips(&replaced);
    assert_eq!(
        serde_json::to_value(&replaced).expect("subscriptions serialize")["replaced_by"],
        json!(PODCAST_HARDCORE_HISTORY)
    );
}

#[test]
fn episode_state_wire_shape() {
    insta::assert_json_snapshot!(atp_612_state());
    assert_round_trips(&atp_612_state());
}

#[test]
fn setting_wire_shape() {
    insta::assert_json_snapshot!(playback_speed_setting());
    assert_round_trips(&playback_speed_setting());
}

#[test]
fn unknown_setting_values_survive_unchanged() {
    let setting = Setting {
        key: "theme".into(),
        value: json!({ "accent": "#ff6600", "dark": true }),
        ..playback_speed_setting()
    };
    assert_round_trips(&setting);
}

// ==============================================================================
// Auth
// ==============================================================================

#[test]
fn auth_request_wire_shape() {
    let request = AuthRequest {
        username: "alice".into(),
        password: "correct horse battery staple".into(),
        device_name: "Pixel 8".into(),
    };
    insta::assert_json_snapshot!(request);
    assert_round_trips(&request);
    assert!(!format!("{request:?}").contains("horse"));
}

#[test]
fn token_response_wire_shape() {
    let response = TokenResponse {
        token: "q3Vd0x8n2kq0bS9eYV5fM1u7cW2jT4pR6aH8gL0zXyE".into(),
        device_id: DEVICE_PIXEL,
        user_id: USER_ALICE,
        expires_at: at("2027-01-22T10:00:00Z"),
    };
    insta::assert_json_snapshot!(response);
    assert_round_trips(&response);
    assert!(!format!("{response:?}").contains("q3Vd"));
}

#[test]
fn me_response_wire_shape() {
    let response = MeResponse {
        user_id: USER_ALICE,
        username: "alice".into(),
        device_id: DEVICE_PIXEL,
    };
    insta::assert_json_snapshot!(response);
    assert_round_trips(&response);
}

// ==============================================================================
// Mutations
// ==============================================================================

#[test]
fn mutation_wire_shapes() {
    let mutations = one_mutation_of_each_type();
    insta::assert_json_snapshot!(mutations);
    for mutation in &mutations {
        assert_round_trips(mutation);
        let json = serde_json::to_value(mutation).expect("mutations serialize");
        assert_eq!(json["type"], mutation.type_name());
        assert_eq!(json["id"], json!(mutation.id()));
        assert_eq!(json["at"], json!(mutation.at()));
        assert_eq!(json["dev"], json!(mutation.dev()));
    }
}

/// A valid `episode.position` with `overrides` applied; a `null` override
/// removes the field.
fn position(overrides: serde_json::Value) -> serde_json::Value {
    let mut raw = json!({
        "id": MUTATION_1,
        "at": "2026-10-24T09:12:03.120Z",
        "dev": DEVICE_PIXEL,
        "type": "episode.position",
        "episode_id": EPISODE_ATP_612,
        "position_ms": 1_234_000,
    });
    let object = raw.as_object_mut().expect("raw is an object");
    for (key, value) in overrides.as_object().expect("overrides are an object") {
        if value.is_null() {
            object.remove(key);
        } else {
            object.insert(key.clone(), value.clone());
        }
    }
    raw
}

fn parse_one(
    raw: serde_json::Value,
) -> Result<Result<Mutation, MutationRejection>, MissingMutationId> {
    let request = SyncRequest {
        mutations: vec![raw],
        ..SyncRequest::default()
    };
    request
        .parse_mutations()
        .map(|mut parsed| parsed.pop().expect("one mutation in, one out"))
}

fn rejection(raw: serde_json::Value) -> MutationRejection {
    parse_one(raw)
        .expect("the id is valid")
        .expect_err("the mutation is invalid")
}

#[test]
fn valid_mutation_parses() {
    let parsed = parse_one(position(json!({})))
        .expect("the id is valid")
        .expect("the mutation is valid");
    assert_eq!(parsed.id(), MUTATION_1);
}

#[test]
fn each_kind_of_bad_mutation_is_a_rejection_carrying_its_id() {
    let cases = [
        ("unknown type", json!({ "type": "episode.starred" })),
        ("missing type", json!({ "type": null })),
        ("missing field", json!({ "position_ms": null })),
        ("ill-typed field", json!({ "position_ms": "20 minutes" })),
        ("fractional position", json!({ "position_ms": 1.5 })),
        ("negative position", json!({ "position_ms": -1 })),
        ("bad episode id", json!({ "episode_id": "atp-612" })),
        ("bad at", json!({ "at": "yesterday" })),
        ("missing at", json!({ "at": null })),
        ("bad dev", json!({ "dev": 7 })),
    ];
    for (name, overrides) in cases {
        let rejection = rejection(position(overrides));
        assert_eq!(rejection.id, MUTATION_1, "{name}");
        assert!(!rejection.reason.is_empty(), "{name}");
    }
}

#[test]
fn negative_position_names_the_problem() {
    let rejection = rejection(position(json!({ "position_ms": -1 })));
    assert!(
        rejection.reason.contains("negative"),
        "{}",
        rejection.reason
    );
}

#[test]
fn rejection_keeps_a_readable_at() {
    let readable = rejection(position(json!({ "position_ms": -1 })));
    assert_eq!(readable.at, Some(at("2026-10-24T09:12:03.120Z")));
    let unreadable = rejection(position(json!({ "at": "yesterday" })));
    assert_eq!(unreadable.at, None);
}

#[test]
fn setting_value_must_be_present_but_may_be_null() {
    let setting = |value: Option<serde_json::Value>| {
        let mut raw = json!({
            "id": MUTATION_2,
            "at": "2026-10-24T09:12:03.120Z",
            "dev": DEVICE_LAPTOP,
            "type": "setting.set",
            "key": "playback_speed",
        });
        if let Some(value) = value {
            raw["value"] = value;
        }
        parse_one(raw).expect("the id is valid")
    };
    assert!(setting(None).is_err(), "a missing value is a rejection");
    assert!(setting(Some(serde_json::Value::Null)).is_ok());
}

#[test]
fn missing_or_malformed_id_breaks_the_envelope() {
    for id in [None, Some(json!("not-a-uuid")), Some(json!(42))] {
        let mut raw = position(json!({ "id": null }));
        if let Some(id) = id {
            raw["id"] = id;
        }
        assert_eq!(parse_one(raw), Err(MissingMutationId { index: 0 }));
    }
}
