//! `/v1/sonos/*` bodies: playback handed to a Sonos group (spec 13.2).

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;

use crate::EpisodeId;
use crate::mutation::non_negative;
use crate::schema::{self, Variant};

/// The response to `GET /v1/sonos/players`. Empty when the server has no
/// `sonos_hosts`, which tells clients to hide Sonos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SonosPlayersResponse {
    pub players: Vec<SonosPlayer>,
}

/// A Sonos group, which plays as one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SonosPlayer {
    /// The group coordinator's UUID (`RINCON_…`).
    pub id: String,
    /// As the Sonos app names the group: "Couch", or "Couch + Küche".
    pub name: String,
    /// The group volume, 0 to 100.
    #[schema(maximum = 100)]
    pub volume: u8,
    pub state: SonosPlayerState,
    /// Set exactly when `state` is `pod`.
    pub session: Option<SonosSessionView>,
}

/// What a group is doing, from the requesting user's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SonosPlayerState {
    /// Stopped or empty, free to take.
    Idle,
    /// Plays or holds something else, another user's Pod session included.
    Other,
    /// Plays or holds this user's Pod session.
    Pod,
}

/// The requesting user's session on a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SonosSessionView {
    pub episode_id: EpisodeId,
    pub playing: bool,
    pub position_ms: i64,
}

/// `POST /v1/sonos/players/{id}/play`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SonosPlayRequest {
    pub episode_id: EpisodeId,
    /// The start position by the client's own state (spec 8).
    #[serde(deserialize_with = "non_negative")]
    #[schema(minimum = 0)]
    pub position_ms: i64,
}

/// `POST /v1/sonos/players/{id}/command`. Every command except `volume`
/// needs the user's own session on the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SonosCommand {
    Pause,
    /// Play from the current position at 1x.
    Resume,
    Seek {
        #[serde(deserialize_with = "non_negative")]
        position_ms: i64,
    },
    /// Seek relative to the speaker's position, clamped at 0. Clients send
    /// `skip_back_ms` negated or `skip_forward_ms`.
    Skip {
        delta_ms: i64,
    },
    Volume {
        #[serde(deserialize_with = "volume_percent")]
        volume: u8,
    },
}

/// The response to `POST /v1/sonos/players/{id}/release`: the session as it
/// was when released, so the client continues from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SonosReleased {
    pub episode_id: EpisodeId,
    pub position_ms: i64,
    pub playing: bool,
}

fn volume_percent<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let volume = u8::deserialize(deserializer)?;
    if volume > 100 {
        return Err(serde::de::Error::custom(format!(
            "volume must be 0 to 100, got {volume}"
        )));
    }
    Ok(volume)
}

// ==============================================================================
// OpenAPI
// ==============================================================================

// `SonosCommand` is internally tagged, which utoipa's derive can't express,
// so each variant gets a payload struct that exists only for its schema,
// the way `Mutation`'s variants have one (`schema::register` adds `type`).
// `tests/openapi.rs` validates every command against the result, which
// catches these drifting from the enum.

/// `pause`: pause the group.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SonosPause {}

/// `resume`: play from the current position at 1x.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SonosResume {}

/// `seek`: seek to a position.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SonosSeek {
    #[schema(minimum = 0)]
    position_ms: i64,
}

/// `skip`: seek relative to the speaker's position, clamped at 0.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SonosSkip {
    delta_ms: i64,
}

/// `volume`: set the group volume. Needs no session.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SonosVolume {
    #[schema(maximum = 100)]
    volume: u8,
}

fn command_variants() -> Vec<Variant> {
    vec![
        Variant::of::<SonosPause>("pause"),
        Variant::of::<SonosResume>("resume"),
        Variant::of::<SonosSeek>("seek"),
        Variant::of::<SonosSkip>("skip"),
        Variant::of::<SonosVolume>("volume"),
    ]
}

impl utoipa::PartialSchema for SonosCommand {
    fn schema() -> RefOr<Schema> {
        schema::one_of("type", &command_variants())
    }
}

impl ToSchema for SonosCommand {
    fn schemas(out: &mut Vec<(String, RefOr<Schema>)>) {
        schema::register("type", command_variants(), out);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn commands_are_tagged_by_type() {
        let commands = [
            (SonosCommand::Pause, json!({ "type": "pause" })),
            (SonosCommand::Resume, json!({ "type": "resume" })),
            (
                SonosCommand::Seek {
                    position_ms: 683_000,
                },
                json!({ "type": "seek", "position_ms": 683_000 }),
            ),
            (
                SonosCommand::Skip { delta_ms: -15_000 },
                json!({ "type": "skip", "delta_ms": -15_000 }),
            ),
            (
                SonosCommand::Volume { volume: 100 },
                json!({ "type": "volume", "volume": 100 }),
            ),
        ];
        for (command, wire) in commands {
            assert_eq!(
                serde_json::to_value(command).expect("commands serialize"),
                wire
            );
            assert_eq!(
                serde_json::from_value::<SonosCommand>(wire).expect("commands deserialize"),
                command
            );
        }
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        for invalid in [
            json!({ "type": "volume", "volume": 101 }),
            json!({ "type": "volume", "volume": -1 }),
            json!({ "type": "seek", "position_ms": -1 }),
            json!({ "type": "skip" }),
            json!({ "type": "rewind" }),
        ] {
            assert!(
                serde_json::from_value::<SonosCommand>(invalid.clone()).is_err(),
                "{invalid}"
            );
        }
        let negative_start = json!({
            "episode_id": "0195b3a0-0000-7000-8000-000000000001",
            "position_ms": -5
        });
        assert!(serde_json::from_value::<SonosPlayRequest>(negative_start).is_err());
    }

    #[test]
    fn player_wire_shape() {
        let player = SonosPlayer {
            id: "RINCON_804AF246458401400".into(),
            name: "Couch".into(),
            volume: 10,
            state: SonosPlayerState::Idle,
            session: None,
        };
        insta::assert_json_snapshot!(player, @r#"
        {
          "id": "RINCON_804AF246458401400",
          "name": "Couch",
          "volume": 10,
          "state": "idle",
          "session": null
        }
        "#);
    }
}
