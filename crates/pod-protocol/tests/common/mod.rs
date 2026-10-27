//! Realistic ids, sample bodies and helpers shared by the wire tests.

// Each test binary uses a different subset.
#![allow(dead_code)]

use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::uuid;

use pod_protocol::*;

pub const USER_ALICE: UserId = UserId::from_uuid(uuid!("0192e7c4-1a2b-7c3d-8e4f-5a6b7c8d9e0f"));
pub const DEVICE_PIXEL: DeviceId =
    DeviceId::from_uuid(uuid!("0192e7c4-5d6e-7f80-9a1b-2c3d4e5f6a7b"));
pub const DEVICE_LAPTOP: DeviceId =
    DeviceId::from_uuid(uuid!("0192e9a0-0b1c-7d2e-8f3a-4b5c6d7e8f90"));
pub const PODCAST_ACCIDENTAL_TECH: PodcastId =
    PodcastId::from_uuid(uuid!("9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c"));
pub const PODCAST_HARDCORE_HISTORY: PodcastId =
    PodcastId::from_uuid(uuid!("a71b2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c3d"));
pub const EPISODE_ATP_612: EpisodeId =
    EpisodeId::from_uuid(uuid!("4b1e7a2c-3d5f-5e6a-8b9c-0d1e2f3a4b5c"));
pub const EPISODE_ATP_611: EpisodeId =
    EpisodeId::from_uuid(uuid!("5c2f8b3d-4e6a-5f7b-9cad-1e2f3a4b5c6d"));
pub const MUTATION_1: MutationId =
    MutationId::from_uuid(uuid!("0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f01"));
pub const MUTATION_2: MutationId =
    MutationId::from_uuid(uuid!("0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f02"));
pub const MUTATION_3: MutationId =
    MutationId::from_uuid(uuid!("0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f03"));
pub const MUTATION_4: MutationId =
    MutationId::from_uuid(uuid!("0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f04"));
pub const MUTATION_5: MutationId =
    MutationId::from_uuid(uuid!("0192f0a1-7b2c-7d3e-8f4a-5b6c7d8e9f05"));

pub fn at(rfc3339: &str) -> Timestamp {
    rfc3339.parse().expect("test literal is RFC 3339")
}

/// Asserts `deserialize(serialize(value)) == value`.
pub fn assert_round_trips<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let json = serde_json::to_string(value).expect("wire types serialize");
    let back: T = serde_json::from_str(&json).expect("serialized wire types deserialize");
    assert_eq!(&back, value, "round trip changed the value: {json}");
}

pub fn atp_podcast() -> Podcast {
    Podcast {
        id: PODCAST_ACCIDENTAL_TECH,
        feed_url: "https://atp.fm/rss".into(),
        status: FeedStatus::Ok,
        title: Some("Accidental Tech Podcast".into()),
        author: Some("Marco Arment, Casey Liss, John Siracusa".into()),
        description: Some(
            "Three nerds discussing tech, Apple, programming, and loosely related matters.".into(),
        ),
        link: Some("https://atp.fm".into()),
        artwork_url: Some("https://cdn.atp.fm/artwork.jpg".into()),
        language: Some("en".into()),
        version: 812,
    }
}

pub fn atp_episode() -> Episode {
    Episode {
        id: EPISODE_ATP_612,
        podcast_id: PODCAST_ACCIDENTAL_TECH,
        title: "612: The Ham Sandwich of Defaults".into(),
        summary_html: "<p>Follow-up on Vision Pro, and a long-awaited keyboard.</p>".into(),
        content_html: "<p>Follow-up on Vision Pro.</p><ul><li><a href=\"https://atp.fm/612\" \
                       rel=\"noopener noreferrer nofollow\">Show notes</a></li></ul>"
            .into(),
        link: None,
        published_at: at("2026-10-20T05:00:00Z"),
        duration_ms: Some(3_612_000),
        audio_url: "https://traffic.libsyn.com/atpfm/atp612.mp3".into(),
        audio_type: "audio/mpeg".into(),
        audio_bytes: Some(57_800_000),
        artwork_url: None,
        season: None,
        number: Some(612),
        version: 811,
    }
}

pub fn atp_subscription() -> Subscription {
    Subscription {
        podcast_id: PODCAST_ACCIDENTAL_TECH,
        feed_url: "https://atp.fm/rss".into(),
        subscribed: true,
        subscribed_at: at("2026-10-01T18:00:00Z"),
        subscribed_dev: DEVICE_PIXEL,
        replaced_by: None,
        version: 1830,
    }
}

pub fn atp_612_state() -> EpisodeState {
    EpisodeState {
        episode_id: EPISODE_ATP_612,
        podcast_id: PODCAST_ACCIDENTAL_TECH,
        position_ms: 1_234_000,
        position_at: Some(at("2026-10-24T09:12:03.120Z")),
        position_dev: Some(DEVICE_PIXEL),
        played: false,
        played_at: None,
        played_dev: None,
        archived: false,
        archived_at: None,
        archived_dev: None,
        version: 1843,
    }
}

pub fn playback_speed_setting() -> Setting {
    Setting {
        key: "playback_speed".into(),
        value: serde_json::json!(1.3),
        value_at: at("2026-10-22T20:41:09.507Z"),
        value_dev: DEVICE_LAPTOP,
        version: 1790,
    }
}

/// One mutation of each type, all valid.
pub fn one_mutation_of_each_type() -> Vec<Mutation> {
    let at = at("2026-10-24T09:12:03.120Z");
    vec![
        Mutation::SubscriptionSet(SubscriptionSet {
            id: MUTATION_1,
            at,
            dev: DEVICE_PIXEL,
            podcast_id: PODCAST_HARDCORE_HISTORY,
            feed_url: "https://feeds.feedburner.com/dancarlin/history".into(),
            subscribed: true,
        }),
        Mutation::EpisodePosition(EpisodePosition {
            id: MUTATION_2,
            at,
            dev: DEVICE_PIXEL,
            episode_id: EPISODE_ATP_612,
            position_ms: 1_234_000,
        }),
        Mutation::EpisodePlayed(EpisodePlayed {
            id: MUTATION_3,
            at,
            dev: DEVICE_PIXEL,
            episode_id: EPISODE_ATP_611,
            played: true,
        }),
        Mutation::EpisodeArchived(EpisodeArchived {
            id: MUTATION_4,
            at,
            dev: DEVICE_LAPTOP,
            episode_id: EPISODE_ATP_611,
            archived: true,
        }),
        Mutation::SettingSet(SettingSet {
            id: MUTATION_5,
            at,
            dev: DEVICE_LAPTOP,
            key: "playback_speed".into(),
            value: serde_json::json!(1.3),
        }),
    ]
}

pub fn sample_request() -> SyncRequest {
    SyncRequest {
        mutations: one_mutation_of_each_type()
            .iter()
            .map(|mutation| serde_json::to_value(mutation).expect("mutations serialize"))
            .collect(),
        scopes: Some(ScopeCursors {
            user: 1841,
            podcasts: vec![
                PodcastCursor {
                    id: PODCAST_ACCIDENTAL_TECH,
                    version: 5523,
                },
                PodcastCursor {
                    id: PODCAST_HARDCORE_HISTORY,
                    version: 0,
                },
            ],
        }),
        limit: Some(500),
    }
}

pub fn sample_response() -> SyncResponse {
    SyncResponse {
        server_time: at("2026-10-24T09:12:04.317Z"),
        results: vec![
            MutationResult {
                id: MUTATION_1,
                outcome: MutationOutcome::Applied {
                    at: at("2026-10-24T09:12:03.120Z"),
                },
            },
            MutationResult {
                id: MUTATION_2,
                outcome: MutationOutcome::Superseded {
                    at: at("2026-10-24T09:12:03.120Z"),
                },
            },
            MutationResult {
                id: MUTATION_3,
                outcome: MutationOutcome::Rejected {
                    at: Some(at("2026-10-24T09:12:03.120Z")),
                    code: RejectionCode::UnknownEpisode,
                },
            },
        ],
        scopes: Some(ScopeChanges {
            user: UserScope {
                version: 1843,
                complete: true,
                subscriptions: vec![atp_subscription()],
                episode_states: vec![atp_612_state()],
                settings: vec![playback_speed_setting()],
            },
            podcasts: vec![
                PodcastScope::Ok(PodcastChanges {
                    id: PODCAST_ACCIDENTAL_TECH,
                    version: 900,
                    complete: false,
                    podcast: Some(atp_podcast()),
                    episodes: vec![
                        ScopeEpisode::Live(atp_episode()),
                        ScopeEpisode::Deleted(EpisodeTombstone {
                            id: EPISODE_ATP_611,
                            version: 900,
                        }),
                    ],
                }),
                PodcastScope::NotSubscribed(NotSubscribedScope {
                    id: PODCAST_HARDCORE_HISTORY,
                }),
            ],
        }),
    }
}
