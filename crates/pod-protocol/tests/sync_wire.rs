//! `POST /v1/sync` bodies as a client and the server see them.

mod common;

use common::*;
use pod_protocol::*;
use serde_json::json;

#[test]
fn one_malformed_mutation_is_rejected_and_the_rest_parse() {
    let request: SyncRequest = serde_json::from_value(json!({
        "mutations": [
            {
                "id": "0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f01",
                "at": "2026-10-24T09:12:03.120Z",
                "dev": "0192e7c4-5d6e-7f80-9a1b-2c3d4e5f6a7b",
                "type": "episode.position",
                "episode_id": "4b1e7a2c-3d5f-5e6a-8b9c-0d1e2f3a4b5c",
                "position_ms": 1234000
            },
            {
                "id": "0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f02",
                "at": "2026-10-24T09:12:04.000Z",
                "dev": "0192e7c4-5d6e-7f80-9a1b-2c3d4e5f6a7b",
                "type": "episode.starred",
                "episode_id": "4b1e7a2c-3d5f-5e6a-8b9c-0d1e2f3a4b5c"
            },
            {
                "id": "0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f03",
                "at": "2026-10-24T09:12:05.000Z",
                "dev": "0192e7c4-5d6e-7f80-9a1b-2c3d4e5f6a7b",
                "type": "episode.played",
                "episode_id": "4b1e7a2c-3d5f-5e6a-8b9c-0d1e2f3a4b5c",
                "played": true
            }
        ],
        "scopes": { "user": 1841 }
    }))
    .expect("a malformed mutation doesn't break the envelope");

    let parsed = request
        .parse_mutations()
        .expect("every mutation has a valid id");
    assert_eq!(parsed.len(), 3);
    assert!(matches!(parsed[0], Ok(Mutation::EpisodePosition(_))));
    assert!(matches!(parsed[2], Ok(Mutation::EpisodePlayed(_))));

    let rejection = parsed[1].as_ref().expect_err("unknown type is rejected");
    assert!(
        rejection.reason.contains("episode.starred"),
        "{}",
        rejection.reason
    );
    insta::assert_json_snapshot!(rejection.result(), @r#"
    {
      "id": "0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f02",
      "status": "rejected",
      "at": "2026-10-24T09:12:04.000Z",
      "code": "invalid_mutation"
    }
    "#);
}

#[test]
fn a_mutation_without_a_valid_id_breaks_the_envelope() {
    let request: SyncRequest = serde_json::from_value(json!({
        "mutations": [
            { "id": MUTATION_1, "at": "2026-10-24T09:12:03.120Z", "dev": DEVICE_PIXEL,
              "type": "episode.played", "episode_id": EPISODE_ATP_612, "played": true },
            { "at": "2026-10-24T09:12:03.120Z", "dev": DEVICE_PIXEL,
              "type": "episode.played", "episode_id": EPISODE_ATP_612, "played": true },
        ]
    }))
    .expect("the envelope itself parses");
    assert_eq!(
        request.parse_mutations(),
        Err(MissingMutationId { index: 1 })
    );
}

#[test]
fn rejection_with_an_unreadable_at_omits_at() {
    let request: SyncRequest = serde_json::from_value(json!({
        "mutations": [
            { "id": MUTATION_1, "at": "last tuesday", "dev": DEVICE_PIXEL,
              "type": "episode.played", "episode_id": EPISODE_ATP_612, "played": true },
        ]
    }))
    .expect("the envelope itself parses");
    let parsed = request.parse_mutations().expect("the id is valid");
    let rejection = parsed[0].as_ref().expect_err("the at is unreadable");
    insta::assert_json_snapshot!(rejection.result(), @r#"
    {
      "id": "0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f01",
      "status": "rejected",
      "code": "invalid_mutation"
    }
    "#);
}

#[test]
fn request_wire_shape() {
    let request = sample_request();
    insta::assert_json_snapshot!(request);
    assert_round_trips(&request);
    assert!(
        request
            .parse_mutations()
            .expect("ids are valid")
            .iter()
            .all(Result::is_ok)
    );
}

#[test]
fn pull_only_and_push_only_requests() {
    let pull_only: SyncRequest = serde_json::from_value(json!({
        "mutations": [],
        "scopes": { "user": 0, "podcasts": [] }
    }))
    .expect("pull-only request parses");
    assert!(pull_only.mutations.is_empty());
    assert_round_trips(&pull_only);

    let push_only: SyncRequest =
        serde_json::from_value(json!({ "mutations": [] })).expect("push-only request parses");
    assert_eq!(push_only.scopes, None);
    insta::assert_json_snapshot!(push_only, @r#"
    {
      "mutations": []
    }
    "#);
}

#[test]
fn effective_limit_defaults_and_clamps() {
    let with_limit = |limit| SyncRequest {
        limit,
        ..SyncRequest::default()
    };
    assert_eq!(with_limit(None).effective_limit(), DEFAULT_SYNC_LIMIT);
    assert_eq!(with_limit(Some(50)).effective_limit(), 50);
    assert_eq!(with_limit(Some(10_000)).effective_limit(), MAX_SYNC_LIMIT);
    assert_eq!(with_limit(Some(0)).effective_limit(), 1);
}

#[test]
fn response_wire_shape() {
    let response = sample_response();
    insta::assert_json_snapshot!(response);
    assert_round_trips(&response);
}

#[test]
fn every_result_status_round_trips() {
    let results = [
        MutationResult {
            id: MUTATION_1,
            outcome: MutationOutcome::Applied {
                at: at("2026-10-24T09:12:03.120Z"),
            },
        },
        MutationResult {
            id: MUTATION_2,
            outcome: MutationOutcome::Superseded {
                at: at("2026-10-24T09:13:03.120Z"),
            },
        },
        MutationResult {
            id: MUTATION_3,
            outcome: MutationOutcome::Rejected {
                at: Some(at("2026-10-24T09:12:03.120Z")),
                code: RejectionCode::UnknownEpisode,
            },
        },
        MutationResult {
            id: MUTATION_3,
            outcome: MutationOutcome::Rejected {
                at: None,
                code: RejectionCode::InvalidFeedUrl,
            },
        },
    ];
    insta::assert_json_snapshot!(results);
    for result in &results {
        assert_round_trips(result);
    }
}

/// A rejection code added by a newer server doesn't break the response; the
/// client treats it like any other `rejected` (spec 4.3).
#[test]
fn unknown_rejection_code_parses_and_round_trips() {
    let wire = json!({
        "server_time": "2026-10-24T09:12:04.000Z",
        "results": [
            {
                "id": "0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f01",
                "status": "rejected",
                "at": "2026-10-24T09:12:03.120Z",
                "code": "episode_archived"
            }
        ]
    });
    let response: SyncResponse =
        serde_json::from_value(wire.clone()).expect("unknown rejection codes parse");
    assert_eq!(
        serde_json::to_value(&response).expect("sync responses serialize"),
        wire
    );
}

#[test]
fn podcast_scope_variants() {
    let scopes = [
        PodcastScope::NotSubscribed(NotSubscribedScope {
            id: PODCAST_HARDCORE_HISTORY,
        }),
        PodcastScope::Ok(PodcastChanges {
            id: PODCAST_ACCIDENTAL_TECH,
            version: 5523,
            complete: false,
            podcast: None,
            episodes: vec![],
        }),
    ];
    insta::assert_json_snapshot!(scopes, @r#"
    [
      {
        "status": "not_subscribed",
        "id": "a71b2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c3d"
      },
      {
        "status": "ok",
        "id": "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c",
        "version": 5523,
        "complete": false
      }
    ]
    "#);
    for scope in &scopes {
        assert_round_trips(scope);
    }
}
