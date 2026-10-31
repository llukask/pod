//! Client mutations pushed through `POST /v1/sync` (spec section 4.3).
//!
//! Every mutation carries `id`, `at` and `dev` next to its `type` and the
//! fields that type writes. Each type is its own struct so the generated
//! OpenAPI has one flat component per type, discriminated by `type`.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;

use crate::schema::{self, Variant};
use crate::{DeviceId, EpisodeId, MutationId, PodcastId, Timestamp};

/// A user action to apply on the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Mutation {
    #[serde(rename = "subscription.set")]
    SubscriptionSet(SubscriptionSet),
    #[serde(rename = "episode.position")]
    EpisodePosition(EpisodePosition),
    #[serde(rename = "episode.played")]
    EpisodePlayed(EpisodePlayed),
    #[serde(rename = "episode.archived")]
    EpisodeArchived(EpisodeArchived),
    #[serde(rename = "setting.set")]
    SettingSet(SettingSet),
}

/// `subscription.set`: create or update a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SubscriptionSet {
    pub id: MutationId,
    pub at: Timestamp,
    pub dev: DeviceId,
    pub podcast_id: PodcastId,
    /// As the user entered it. The server normalizes it and rejects it with
    /// `invalid_feed_url` if that fails.
    pub feed_url: String,
    pub subscribed: bool,
}

/// `episode.position`: set the playback position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodePosition {
    pub id: MutationId,
    pub at: Timestamp,
    pub dev: DeviceId,
    pub episode_id: EpisodeId,
    /// Never negative. There is no upper bound: feed durations are often
    /// wrong, and a real position past the stated duration must still sync.
    #[serde(deserialize_with = "non_negative")]
    #[schema(minimum = 0)]
    pub position_ms: i64,
}

/// `episode.played`: mark played or unplayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodePlayed {
    pub id: MutationId,
    pub at: Timestamp,
    pub dev: DeviceId,
    pub episode_id: EpisodeId,
    pub played: bool,
}

/// `episode.archived`: hide an episode from the inbox or return it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeArchived {
    pub id: MutationId,
    pub at: Timestamp,
    pub dev: DeviceId,
    pub episode_id: EpisodeId,
    pub archived: bool,
}

/// `setting.set`: set a synced setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SettingSet {
    pub id: MutationId,
    pub at: Timestamp,
    pub dev: DeviceId,
    pub key: String,
    /// Any JSON value, `null` included, but the field must be present.
    #[serde(deserialize_with = "present_value")]
    #[schema(value_type = Value)]
    pub value: serde_json::Value,
}

pub(crate) fn non_negative<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = i64::deserialize(deserializer)?;
    if value < 0 {
        return Err(serde::de::Error::custom(format!(
            "position_ms must not be negative, got {value}"
        )));
    }
    Ok(value)
}

/// serde fills a missing `serde_json::Value` field with `null`; going through
/// `deserialize_with` makes a missing `value` an error instead.
fn present_value<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<serde_json::Value, D::Error> {
    serde_json::Value::deserialize(deserializer)
}

impl Mutation {
    pub fn id(&self) -> MutationId {
        match self {
            Self::SubscriptionSet(m) => m.id,
            Self::EpisodePosition(m) => m.id,
            Self::EpisodePlayed(m) => m.id,
            Self::EpisodeArchived(m) => m.id,
            Self::SettingSet(m) => m.id,
        }
    }

    /// When the user acted, by the client's clock.
    pub fn at(&self) -> Timestamp {
        match self {
            Self::SubscriptionSet(m) => m.at,
            Self::EpisodePosition(m) => m.at,
            Self::EpisodePlayed(m) => m.at,
            Self::EpisodeArchived(m) => m.at,
            Self::SettingSet(m) => m.at,
        }
    }

    /// The device the client stamped the change with, the LWW tiebreaker.
    pub fn dev(&self) -> DeviceId {
        match self {
            Self::SubscriptionSet(m) => m.dev,
            Self::EpisodePosition(m) => m.dev,
            Self::EpisodePlayed(m) => m.dev,
            Self::EpisodeArchived(m) => m.dev,
            Self::SettingSet(m) => m.dev,
        }
    }

    /// The `type` tag on the wire.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::SubscriptionSet(_) => "subscription.set",
            Self::EpisodePosition(_) => "episode.position",
            Self::EpisodePlayed(_) => "episode.played",
            Self::EpisodeArchived(_) => "episode.archived",
            Self::SettingSet(_) => "setting.set",
        }
    }
}

fn mutation_variants() -> Vec<Variant> {
    vec![
        Variant::of::<SubscriptionSet>("subscription.set"),
        Variant::of::<EpisodePosition>("episode.position"),
        Variant::of::<EpisodePlayed>("episode.played"),
        Variant::of::<EpisodeArchived>("episode.archived"),
        Variant::of::<SettingSet>("setting.set"),
    ]
}

impl utoipa::PartialSchema for Mutation {
    fn schema() -> RefOr<Schema> {
        schema::one_of("type", &mutation_variants())
    }
}

impl ToSchema for Mutation {
    fn schemas(out: &mut Vec<(String, RefOr<Schema>)>) {
        schema::register("type", mutation_variants(), out);
    }
}

/// A mutation that can't be applied, reported in the results as `rejected`
/// with code `invalid_mutation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRejection {
    pub id: MutationId,
    /// The mutation's `at` if that field alone was readable, so the result
    /// can echo it like every other result does.
    pub at: Option<Timestamp>,
    /// What was wrong, for the server log. Not sent to the client.
    pub reason: String,
}

/// A mutation whose `id` is missing or not a UUID. Results are keyed by id,
/// so such a mutation can't be rejected on its own and the whole request is
/// a `bad_request`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("mutation {index} has no valid `id`")]
pub struct MissingMutationId {
    /// The mutation's position in the request.
    pub index: usize,
}

/// Parses one raw mutation from a sync request.
///
/// Anything wrong besides the id (unknown `type`, a missing or ill-typed
/// field, an unreadable `at` or `dev`, a negative `position_ms`) becomes a
/// [`MutationRejection`]. Checks that need the database, such as feed URL
/// validity or whether an episode exists, are the server's.
pub(crate) fn parse_mutation(
    index: usize,
    raw: &serde_json::Value,
) -> Result<Result<Mutation, MutationRejection>, MissingMutationId> {
    let id = raw
        .get("id")
        .and_then(|id| MutationId::deserialize(id).ok())
        .ok_or(MissingMutationId { index })?;
    Ok(
        Mutation::deserialize(raw).map_err(|error| MutationRejection {
            id,
            at: raw.get("at").and_then(|at| Timestamp::deserialize(at).ok()),
            reason: error.to_string(),
        }),
    )
}
