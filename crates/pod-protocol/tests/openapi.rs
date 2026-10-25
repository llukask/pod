//! Keeps the checked-in `openapi.json` current and checks that the wire
//! shapes the other tests snapshot actually match the schemas in it.

mod common;

use std::path::Path;

use common::*;
use pod_protocol::*;
use serde::Serialize;
use serde_json::{Value, json};

const UPDATE_VAR: &str = "POD_UPDATE_OPENAPI";

fn checked_in_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi.json")
}

fn generated() -> String {
    let mut json = openapi::document()
        .to_pretty_json()
        .expect("the OpenAPI document serializes");
    json.push('\n');
    json
}

#[test]
fn checked_in_openapi_json_is_up_to_date() {
    let path = checked_in_path();
    let generated = generated();
    if std::env::var_os(UPDATE_VAR).is_some_and(|value| value == "1") {
        std::fs::write(&path, &generated).expect("openapi.json is writable");
        return;
    }
    let checked_in = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        checked_in == generated,
        "{} is out of date with pod-protocol's types. Regenerate it with\n\n    \
         {UPDATE_VAR}=1 cargo test -p pod-protocol --test openapi\n",
        path.display()
    );
}

/// Validates `body` against `#/components/schemas/{component}`, resolving
/// references within the generated document. OpenAPI 3.1 schemas are JSON
/// Schema 2020-12; the `discriminator` keyword is ignored, so `oneOf` has to
/// separate the variants by their tag properties alone.
fn assert_matches_schema(component: &str, body: &impl Serialize) {
    let mut root: Value = serde_json::from_str(&generated()).expect("the document is valid JSON");
    root["$ref"] = json!(format!("#/components/schemas/{component}"));
    let validator = jsonschema::draft202012::new(&root).expect("the document is a valid schema");
    let instance = serde_json::to_value(body).expect("wire types serialize");
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{component} schema rejects {instance:#}:\n{}",
        errors.join("\n")
    );
}

#[test]
fn sync_bodies_match_their_schemas() {
    assert_matches_schema("SyncRequest", &sample_request());
    assert_matches_schema("SyncResponse", &sample_response());
    for mutation in one_mutation_of_each_type() {
        assert_matches_schema("Mutation", &mutation);
    }
    let rejected_without_at = MutationResult {
        id: MUTATION_1,
        outcome: MutationOutcome::Rejected {
            at: None,
            code: RejectionCode::InvalidMutation,
        },
    };
    assert_matches_schema("MutationResult", &rejected_without_at);
}

#[test]
fn auth_and_error_bodies_match_their_schemas() {
    assert_matches_schema(
        "AuthRequest",
        &AuthRequest {
            username: "alice".into(),
            password: "correct horse battery staple".into(),
            device_name: "Pixel 8".into(),
        },
    );
    assert_matches_schema(
        "TokenResponse",
        &TokenResponse {
            token: "q3Vd0x8n2kq0bS9eYV5fM1u7cW2jT4pR6aH8gL0zXyE".into(),
            device_id: DEVICE_PIXEL,
            user_id: USER_ALICE,
            expires_at: at("2027-01-22T10:00:00Z"),
        },
    );
    assert_matches_schema(
        "MeResponse",
        &MeResponse {
            user_id: USER_ALICE,
            username: "alice".into(),
            device_id: DEVICE_PIXEL,
        },
    );
    for code in ErrorCode::ALL {
        assert_matches_schema("ErrorBody", &ErrorBody::new(code, "Something went wrong."));
    }
}

#[test]
fn directory_bodies_match_their_schemas() {
    let feed_url = "https://feeds.example.org/daily.xml";
    let podcast_id = PodcastId::from_feed_url(
        &normalize_feed_url(feed_url).expect("the sample feed URL is valid"),
    );
    assert_matches_schema(
        "DirectorySearchResponse",
        &DirectorySearchResponse {
            results: vec![
                DirectoryResult {
                    title: "The Daily".into(),
                    author: Some("The New York Times".into()),
                    feed_url: feed_url.into(),
                    artwork_url: Some("https://images.example.org/daily-600.jpg".into()),
                    podcast_id,
                },
                DirectoryResult {
                    title: "Untitled Show".into(),
                    author: None,
                    feed_url: feed_url.into(),
                    artwork_url: None,
                    podcast_id,
                },
            ],
        },
    );
}

/// Guards against a validator that accepts everything, which would make the
/// checks above vacuous.
#[test]
fn schemas_reject_wrong_shapes() {
    let mut root: Value = serde_json::from_str(&generated()).expect("the document is valid JSON");
    root["$ref"] = json!("#/components/schemas/Mutation");
    let validator = jsonschema::draft202012::new(&root).expect("the document is a valid schema");

    let mut no_position =
        serde_json::to_value(&one_mutation_of_each_type()[1]).expect("mutations serialize");
    no_position
        .as_object_mut()
        .expect("mutations are objects")
        .remove("position_ms");
    assert!(!validator.is_valid(&no_position));

    let mut unknown_type =
        serde_json::to_value(&one_mutation_of_each_type()[2]).expect("mutations serialize");
    unknown_type["type"] = json!("episode.starred");
    assert!(!validator.is_valid(&unknown_type));
}
