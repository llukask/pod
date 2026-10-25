//! `POST /v1/sync` request and response (spec section 4.2).

use serde::{Deserialize, Serialize};
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{PartialSchema, ToSchema};

use crate::mutation::{self, MissingMutationId, Mutation, MutationRejection};
use crate::schema::{self, Variant};
use crate::{
    EpisodeState, MutationId, Podcast, PodcastId, ScopeEpisode, Setting, Subscription, Timestamp,
};

/// The most mutations one sync request may carry. More is `batch_too_large`.
pub const MAX_MUTATIONS: usize = 200;

/// The change limit when a request doesn't set `limit`.
pub const DEFAULT_SYNC_LIMIT: u32 = 500;

/// The highest change limit the server honours.
pub const MAX_SYNC_LIMIT: u32 = 2000;

// ==============================================================================
// Request
// ==============================================================================

/// Push and pull in one round trip.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct SyncRequest {
    /// Mutations to apply, in order. Empty for a pull-only request. A
    /// malformed mutation is rejected on its own and doesn't fail the request.
    // Raw JSON so that `parse_mutations` can reject each mutation on its own.
    // Rust clients build these with `serde_json::to_value(&mutation)`.
    #[serde(default)]
    #[schema(value_type = Vec<Mutation>, max_items = 200)]
    pub mutations: Vec<serde_json::Value>,
    /// Cursors to pull from. Absent for a push-only request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<ScopeCursors>,
    /// Caps the total number of changed rows in the response. Defaults to
    /// 500; values above 2000 are treated as 2000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1)]
    pub limit: Option<u32>,
}

impl SyncRequest {
    /// Parses every mutation, in request order.
    ///
    /// The outer error means some mutation has no usable `id`, so the request
    /// can't be answered per mutation and is a `bad_request`. Each inner
    /// `Err` is a mutation to report as rejected with `invalid_mutation`.
    pub fn parse_mutations(
        &self,
    ) -> Result<Vec<Result<Mutation, MutationRejection>>, MissingMutationId> {
        self.mutations
            .iter()
            .enumerate()
            .map(|(index, raw)| mutation::parse_mutation(index, raw))
            .collect()
    }

    /// The change limit to apply: [`DEFAULT_SYNC_LIMIT`] when unset, clamped
    /// to `1..=MAX_SYNC_LIMIT`.
    pub fn effective_limit(&self) -> u32 {
        self.limit
            .unwrap_or(DEFAULT_SYNC_LIMIT)
            .clamp(1, MAX_SYNC_LIMIT)
    }
}

/// The client's cursor in each scope it syncs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScopeCursors {
    /// The highest user-scope version the client has, 0 initially.
    pub user: i64,
    /// Podcast scopes in the order the server should fill them. An array,
    /// not an object, because the order matters.
    #[serde(default)]
    pub podcasts: Vec<PodcastCursor>,
}

/// The client's cursor in one podcast scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PodcastCursor {
    pub id: PodcastId,
    /// The highest version the client has, 0 for a new scope.
    pub version: i64,
}

// ==============================================================================
// Response
// ==============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SyncResponse {
    pub server_time: Timestamp,
    /// One result per request mutation, in request order.
    pub results: Vec<MutationResult>,
    /// Present when the request had `scopes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<ScopeChanges>,
}

/// The outcome of one mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationResult {
    pub id: MutationId,
    #[serde(flatten)]
    pub outcome: MutationOutcome,
}

/// What happened to a mutation, tagged by `status` on the wire.
///
/// `at` is the timestamp the server stored, which differs from the sent one
/// when the server clamped a fast clock (spec 4.4). Clients copy it into
/// their local `*_at` (spec 5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MutationOutcome {
    /// Stored.
    Applied { at: Timestamp },
    /// A newer value already existed for the field.
    Superseded { at: Timestamp },
    /// Will never succeed. `at` is absent only when the mutation's own `at`
    /// was unreadable, since then there is nothing to echo.
    Rejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<Timestamp>,
        code: RejectionCode,
    },
}

/// Why a mutation was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectionCode {
    /// Unknown `type`, a missing field or a bad value.
    InvalidMutation,
    /// An `episode.*` mutation names an episode the server doesn't know.
    UnknownEpisode,
    /// A subscribe whose `feed_url` doesn't normalize.
    InvalidFeedUrl,
}

impl MutationRejection {
    /// The `rejected: invalid_mutation` result for this mutation.
    pub fn result(&self) -> MutationResult {
        MutationResult {
            id: self.id,
            outcome: MutationOutcome::Rejected {
                at: self.at,
                code: RejectionCode::InvalidMutation,
            },
        }
    }
}

/// Code generators handle one flat object better than a union nested in a
/// flattened field, so the schema is written out by hand as the flat shape
/// the wire has. `tests/openapi.rs` validates the snapshots against it.
impl PartialSchema for MutationResult {
    fn schema() -> RefOr<Schema> {
        let status = ObjectBuilder::new()
            .schema_type(Type::String)
            .enum_values(Some(["applied", "superseded", "rejected"]));
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .property("id", Ref::from_schema_name(MutationId::name()))
            .required("id")
            .property("status", status)
            .required("status")
            .property("at", Ref::from_schema_name(Timestamp::name()))
            .property("code", Ref::from_schema_name(RejectionCode::name()))
            .description(Some(
                "`at` is the timestamp the server stored. It is present unless \
                 `status` is `rejected` and the mutation's own `at` was \
                 unreadable. `code` is present exactly when `status` is \
                 `rejected`.",
            ))
            .into()
    }
}

impl ToSchema for MutationResult {
    fn schemas(out: &mut Vec<(String, RefOr<Schema>)>) {
        for (name, schema) in [
            (MutationId::name(), MutationId::schema()),
            (Timestamp::name(), Timestamp::schema()),
            (RejectionCode::name(), RejectionCode::schema()),
        ] {
            out.push((name.into_owned(), schema));
        }
    }
}

/// Changes per scope, user scope first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ScopeChanges {
    pub user: UserScope,
    /// One entry per requested podcast scope, in request order.
    pub podcasts: Vec<PodcastScope>,
}

/// Rows of the user scope with `version` above the request cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UserScope {
    /// The highest version returned, or the request cursor if none.
    pub version: i64,
    /// False when more rows exist beyond `version`; the client asks again.
    pub complete: bool,
    pub subscriptions: Vec<Subscription>,
    pub episode_states: Vec<EpisodeState>,
    pub settings: Vec<Setting>,
}

/// One podcast scope's changes, tagged by `status` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "`Ok` is the common case, boxing it would add an allocation per scope"
)]
pub enum PodcastScope {
    /// The user isn't subscribed; the client drops the scope.
    NotSubscribed(NotSubscribedScope),
    Ok(PodcastChanges),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NotSubscribedScope {
    pub id: PodcastId,
}

/// Rows of one podcast scope with `version` above the request cursor.
///
/// A scope the change limit didn't reach comes back with the request cursor
/// as `version`, `complete: false` and no rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PodcastChanges {
    pub id: PodcastId,
    /// The highest version returned, or the request cursor if none.
    pub version: i64,
    pub complete: bool,
    /// The catalog row, when it changed since the cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub podcast: Option<Podcast>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub episodes: Vec<ScopeEpisode>,
}

impl PodcastScope {
    pub fn id(&self) -> PodcastId {
        match self {
            Self::NotSubscribed(scope) => scope.id,
            Self::Ok(scope) => scope.id,
        }
    }
}

fn podcast_scope_variants() -> Vec<Variant> {
    vec![
        Variant::of::<NotSubscribedScope>("not_subscribed"),
        Variant::of::<PodcastChanges>("ok"),
    ]
}

impl PartialSchema for PodcastScope {
    fn schema() -> RefOr<Schema> {
        schema::one_of("status", &podcast_scope_variants())
    }
}

impl ToSchema for PodcastScope {
    fn schemas(out: &mut Vec<(String, RefOr<Schema>)>) {
        schema::register("status", podcast_scope_variants(), out);
    }
}
