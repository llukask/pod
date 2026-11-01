//! `/v1/connect/*` bodies and events: one playback per account, controlled
//! from any device and moved between devices (spec section 14).

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use uuid::Uuid;

use crate::mutation::non_negative;
use crate::schema::{self, Variant};
use crate::{DeviceId, EpisodeId, PodcastId};

/// What kind of client a device is, from the `kind` query parameter of
/// `GET /v1/connect/events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Web,
    Desktop,
    Android,
}

/// A connected client device (spec 14.2). Sonos groups aren't devices of
/// this list; clients get them from `GET /v1/sonos/players`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConnectDevice {
    pub id: DeviceId,
    /// The session's `device_name`.
    pub name: String,
    pub kind: DeviceKind,
}

/// The playing episode's metadata, so a device that doesn't have the
/// episode locally can show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlaybackEpisode {
    pub id: EpisodeId,
    pub podcast_id: PodcastId,
    pub title: String,
    pub podcast_title: Option<String>,
    /// The episode's own artwork, else the podcast's.
    pub artwork_url: Option<String>,
    pub duration_ms: Option<i64>,
}

/// The account's playback (spec 14.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Playback {
    /// The active device: a client's device id or a Sonos player id
    /// (`RINCON_…`). Null when nothing is active, and then the rest is the
    /// account's last playback, paused.
    pub device_id: Option<String>,
    pub episode: PlaybackEpisode,
    /// The position when the server sent this value. While `playing`,
    /// clients advance it by the elapsed time times `speed`.
    #[schema(minimum = 0)]
    pub position_ms: i64,
    pub playing: bool,
    pub speed: f64,
    /// The Sonos group volume, 0 to 100, while a Sonos group is active.
    #[schema(maximum = 100)]
    pub volume: Option<u8>,
}

impl Playback {
    /// The device id `device_id` names when it is a client device, `None`
    /// for a Sonos group or nothing.
    pub fn client_device(&self) -> Option<DeviceId> {
        self.device_id.as_deref()?.parse().ok()
    }

    /// Whether `device` is the active device.
    pub fn is_active(&self, device: DeviceId) -> bool {
        self.client_device() == Some(device)
    }
}

/// The body of a `state` event and of `GET /v1/connect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConnectState {
    /// The user's connected client devices, in connection order.
    pub devices: Vec<ConnectDevice>,
    /// Null while the user has never played anything.
    pub playback: Option<Playback>,
}

/// `POST /v1/connect/report`: the local player's state, sent by the device
/// that has it (spec 14.3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConnectReport {
    pub episode_id: EpisodeId,
    #[serde(deserialize_with = "non_negative")]
    #[schema(minimum = 0)]
    pub position_ms: i64,
    pub playing: bool,
    /// The playback speed, a finite number above 0.
    #[serde(deserialize_with = "positive_speed")]
    pub speed: f64,
    /// The `transfer_id` of the `release` this report answers.
    #[serde(default)]
    pub released: Option<Uuid>,
}

/// `POST /v1/connect/command`, run on the active device (spec 14.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectCommand {
    Pause,
    /// Play from the current position.
    Resume,
    Seek {
        #[serde(deserialize_with = "non_negative")]
        position_ms: i64,
    },
    /// Seek relative to the current position, clamped at 0.
    Skip {
        delta_ms: i64,
    },
    /// Start another episode on the active device, with the start rule of
    /// spec 8.
    Play {
        episode_id: EpisodeId,
    },
    /// Set the Sonos group volume. Sonos groups only.
    Volume {
        #[serde(deserialize_with = "volume_percent")]
        volume: u8,
    },
}

/// `POST /v1/connect/transfer` (spec 14.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TransferRequest {
    /// A connected client device of the user, or a Sonos player id.
    pub device_id: String,
}

/// One event of `GET /v1/connect/events` (spec 14.2), sent as one
/// `data:` line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectEvent {
    State(ConnectState),
    /// A command for this device, which is the active one.
    Command {
        command: ConnectCommand,
    },
    /// Start the episode here and play it, at `position_ms`, or by the start
    /// rule of spec 8 over local state when it is null.
    Load {
        episode_id: EpisodeId,
        position_ms: Option<i64>,
    },
    /// Stop being the active device: pause, then report with `released`.
    Release {
        transfer_id: Option<Uuid>,
    },
    /// Another stream of the same device took over; stop reconnecting.
    Replaced,
}

impl ConnectEvent {
    /// The event as an SSE `data:` payload.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("connect events serialize")
    }
}

fn positive_speed<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    let speed = f64::deserialize(deserializer)?;
    if !speed.is_finite() || speed <= 0.0 {
        return Err(serde::de::Error::custom(format!(
            "speed must be a finite number above 0, got {speed}"
        )));
    }
    Ok(speed)
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

// Both unions are internally tagged, which utoipa's derive can't express, so
// each variant gets a payload struct that exists only for its schema, as in
// `sonos.rs`. `tests/openapi.rs` validates every variant against the result.

/// `pause`: pause.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectPause {}

/// `resume`: play from the current position.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectResume {}

/// `seek`: seek to a position.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectSeek {
    #[schema(minimum = 0)]
    position_ms: i64,
}

/// `skip`: seek relative to the current position, clamped at 0.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectSkip {
    delta_ms: i64,
}

/// `play`: start another episode on the active device.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectPlay {
    episode_id: EpisodeId,
}

/// `volume`: set the Sonos group volume.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectVolume {
    #[schema(maximum = 100)]
    volume: u8,
}

fn command_variants() -> Vec<Variant> {
    vec![
        Variant::of::<ConnectPause>("pause"),
        Variant::of::<ConnectResume>("resume"),
        Variant::of::<ConnectSeek>("seek"),
        Variant::of::<ConnectSkip>("skip"),
        Variant::of::<ConnectPlay>("play"),
        Variant::of::<ConnectVolume>("volume"),
    ]
}

impl utoipa::PartialSchema for ConnectCommand {
    fn schema() -> RefOr<Schema> {
        schema::one_of("type", &command_variants())
    }
}

impl ToSchema for ConnectCommand {
    fn schemas(out: &mut Vec<(String, RefOr<Schema>)>) {
        schema::register("type", command_variants(), out);
    }
}

/// `state`: the devices and the playback.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectStateEvent {
    devices: Vec<ConnectDevice>,
    playback: Option<Playback>,
}

/// `command`: a command for this device.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectCommandEvent {
    command: ConnectCommand,
}

/// `load`: start this episode here and play it.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectLoadEvent {
    episode_id: EpisodeId,
    /// Null: by the start rule of spec 8 over local state.
    position_ms: Option<i64>,
}

/// `release`: stop being the active device and report with `released`.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectReleaseEvent {
    transfer_id: Option<Uuid>,
}

/// `replaced`: another stream of this device took over.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ConnectReplacedEvent {}

fn event_variants() -> Vec<Variant> {
    vec![
        Variant::of::<ConnectStateEvent>("state"),
        Variant::of::<ConnectCommandEvent>("command"),
        Variant::of::<ConnectLoadEvent>("load"),
        Variant::of::<ConnectReleaseEvent>("release"),
        Variant::of::<ConnectReplacedEvent>("replaced"),
    ]
}

impl utoipa::PartialSchema for ConnectEvent {
    fn schema() -> RefOr<Schema> {
        schema::one_of("type", &event_variants())
    }
}

impl ToSchema for ConnectEvent {
    fn schemas(out: &mut Vec<(String, RefOr<Schema>)>) {
        schema::register("type", event_variants(), out);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const EPISODE: &str = "0195b3a0-0000-7000-8000-000000000001";
    const PODCAST: &str = "0195b3a0-0000-7000-8000-000000000002";
    const DEVICE: &str = "0195b3a0-0000-7000-8000-000000000003";

    fn episode_id() -> EpisodeId {
        EPISODE.parse().expect("test values are valid")
    }

    #[test]
    fn commands_are_tagged_by_type() {
        let commands = [
            (ConnectCommand::Pause, json!({ "type": "pause" })),
            (ConnectCommand::Resume, json!({ "type": "resume" })),
            (
                ConnectCommand::Seek { position_ms: 5 },
                json!({ "type": "seek", "position_ms": 5 }),
            ),
            (
                ConnectCommand::Skip { delta_ms: -15_000 },
                json!({ "type": "skip", "delta_ms": -15_000 }),
            ),
            (
                ConnectCommand::Play {
                    episode_id: episode_id(),
                },
                json!({ "type": "play", "episode_id": EPISODE }),
            ),
            (
                ConnectCommand::Volume { volume: 100 },
                json!({ "type": "volume", "volume": 100 }),
            ),
        ];
        for (command, wire) in commands {
            assert_eq!(
                serde_json::to_value(command).expect("test values are valid"),
                wire
            );
            assert_eq!(
                serde_json::from_value::<ConnectCommand>(wire).expect("test values are valid"),
                command
            );
        }
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        for invalid in [
            json!({ "type": "volume", "volume": 101 }),
            json!({ "type": "seek", "position_ms": -1 }),
            json!({ "type": "play" }),
            json!({ "type": "rewind" }),
        ] {
            assert!(
                serde_json::from_value::<ConnectCommand>(invalid.clone()).is_err(),
                "{invalid}"
            );
        }
        for speed in [json!(0), json!(-1.5), json!("fast")] {
            let report = json!({
                "episode_id": EPISODE, "position_ms": 0, "playing": true, "speed": speed
            });
            assert!(
                serde_json::from_value::<ConnectReport>(report.clone()).is_err(),
                "{report}"
            );
        }
        let negative = json!({
            "episode_id": EPISODE, "position_ms": -1, "playing": true, "speed": 1.0
        });
        assert!(serde_json::from_value::<ConnectReport>(negative).is_err());
    }

    #[test]
    fn report_released_is_optional() {
        let report: ConnectReport = serde_json::from_value(json!({
            "episode_id": EPISODE, "position_ms": 10, "playing": false, "speed": 1.5
        }))
        .expect("test values are valid");
        assert_eq!(report.released, None);
    }

    #[test]
    fn events_wire_shape() {
        let state = ConnectEvent::State(ConnectState {
            devices: vec![ConnectDevice {
                id: DEVICE.parse().expect("test values are valid"),
                name: "Pixel 8".into(),
                kind: DeviceKind::Android,
            }],
            playback: Some(Playback {
                device_id: Some(DEVICE.into()),
                episode: PlaybackEpisode {
                    id: episode_id(),
                    podcast_id: PODCAST.parse().expect("test values are valid"),
                    title: "Episode".into(),
                    podcast_title: Some("Show".into()),
                    artwork_url: None,
                    duration_ms: Some(3_600_000),
                },
                position_ms: 683_000,
                playing: true,
                speed: 1.0,
                volume: None,
            }),
        });
        insta::assert_json_snapshot!(state, @r#"
        {
          "type": "state",
          "devices": [
            {
              "id": "0195b3a0-0000-7000-8000-000000000003",
              "name": "Pixel 8",
              "kind": "android"
            }
          ],
          "playback": {
            "device_id": "0195b3a0-0000-7000-8000-000000000003",
            "episode": {
              "id": "0195b3a0-0000-7000-8000-000000000001",
              "podcast_id": "0195b3a0-0000-7000-8000-000000000002",
              "title": "Episode",
              "podcast_title": "Show",
              "artwork_url": null,
              "duration_ms": 3600000
            },
            "position_ms": 683000,
            "playing": true,
            "speed": 1.0,
            "volume": null
          }
        }
        "#);
        let ConnectEvent::State(parsed) =
            serde_json::from_str::<ConnectEvent>(&state.to_json()).expect("test values are valid")
        else {
            panic!("a state event parses as one");
        };
        assert!(
            parsed
                .playback
                .expect("test values are valid")
                .is_active(DEVICE.parse().expect("test values are valid"))
        );

        assert_eq!(
            serde_json::to_value(ConnectEvent::Command {
                command: ConnectCommand::Pause
            })
            .expect("test values are valid"),
            json!({ "type": "command", "command": { "type": "pause" } })
        );
        assert_eq!(
            serde_json::to_value(ConnectEvent::Load {
                episode_id: episode_id(),
                position_ms: None
            })
            .expect("test values are valid"),
            json!({ "type": "load", "episode_id": EPISODE, "position_ms": null })
        );
        assert_eq!(
            serde_json::to_value(ConnectEvent::Release { transfer_id: None })
                .expect("test values are valid"),
            json!({ "type": "release", "transfer_id": null })
        );
        assert_eq!(
            serde_json::to_value(ConnectEvent::Replaced).expect("test values are valid"),
            json!({ "type": "replaced" })
        );
    }

    #[test]
    fn a_sonos_device_is_no_client_device() {
        let playback = Playback {
            device_id: Some("RINCON_804AF246458401400".into()),
            episode: PlaybackEpisode {
                id: episode_id(),
                podcast_id: PODCAST.parse().expect("test values are valid"),
                title: "Episode".into(),
                podcast_title: None,
                artwork_url: None,
                duration_ms: None,
            },
            position_ms: 0,
            playing: false,
            speed: 1.0,
            volume: Some(10),
        };
        assert_eq!(playback.client_device(), None);
        assert!(!playback.is_active(DEVICE.parse().expect("test values are valid")));
    }
}
