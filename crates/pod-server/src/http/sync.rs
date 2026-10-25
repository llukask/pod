//! `POST /v1/sync` (spec 4.2).

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use color_eyre::eyre::WrapErr;
use pod_protocol::{
    MAX_MUTATIONS, PodcastChanges, PodcastScope, ScopeChanges, ScopeCursors, SyncRequest,
    SyncResponse, UserScope,
};

use super::AppState;
use super::error::{ApiError, ApiJson};
use crate::auth::AuthSession;
use crate::sync;

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/sync", post(sync_handler))
}

async fn sync_handler(
    State(state): State<AppState>,
    session: AuthSession,
    ApiJson(request): ApiJson<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
    if request.mutations.len() > MAX_MUTATIONS {
        return Err(ApiError::batch_too_large(format!(
            "At most {MAX_MUTATIONS} mutations per request, got {}.",
            request.mutations.len()
        )));
    }
    let mutations = request
        .parse_mutations()
        .map_err(|missing| ApiError::bad_request(missing.to_string()))?;
    for rejection in mutations.iter().filter_map(|parsed| parsed.as_ref().err()) {
        tracing::info!(
            user_id = %session.user_id, mutation_id = %rejection.id,
            reason = %rejection.reason, "rejected an invalid mutation"
        );
    }

    let now = state.clock.now();
    let policy = &state.fetch_policy;
    let results = sync::apply_batch(&state.pool, session.user_id, &mutations, now, policy)
        .await
        .context("apply the pushed mutations")?;

    Ok(Json(SyncResponse {
        server_time: now,
        results,
        scopes: request.scopes.as_ref().map(unchanged_scopes),
    }))
}

/// Every requested scope with no changes, at the request's cursors.
///
/// Reporting each scope `complete` keeps a client from looping on a server
/// that doesn't pull yet. An unchanged cursor loses nothing: once pull
/// lands, the next request picks up from the same place.
// TODO(KIL-20): replace with the pull half (spec 4.2 rules 2 to 6).
fn unchanged_scopes(cursors: &ScopeCursors) -> ScopeChanges {
    ScopeChanges {
        user: UserScope {
            version: cursors.user,
            complete: true,
            subscriptions: Vec::new(),
            episode_states: Vec::new(),
            settings: Vec::new(),
        },
        podcasts: cursors
            .podcasts
            .iter()
            .map(|cursor| {
                PodcastScope::Ok(PodcastChanges {
                    id: cursor.id,
                    version: cursor.version,
                    complete: true,
                    podcast: None,
                    episodes: Vec::new(),
                })
            })
            .collect(),
    }
}
