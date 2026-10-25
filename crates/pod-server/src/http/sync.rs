//! `POST /v1/sync` (spec 4.2).

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use color_eyre::eyre::WrapErr;
use pod_protocol::{MAX_MUTATIONS, SyncRequest, SyncResponse};

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
    // A pull-only request skips the push transaction, so it takes no
    // user-scope lock and never waits for another device's push.
    let results = if mutations.is_empty() {
        Vec::new()
    } else {
        let policy = &state.fetch_policy;
        sync::apply_batch(&state.pool, session.user_id, &mutations, now, policy)
            .await
            .context("apply the pushed mutations")?
    };

    // The pull starts after the push committed, so it sees this request's
    // own writes.
    let scopes = match &request.scopes {
        Some(cursors) => Some(
            sync::pull(
                &state.pool,
                session.user_id,
                cursors,
                request.effective_limit(),
            )
            .await
            .context("pull the requested scopes")?,
        ),
        None => None,
    };

    Ok(Json(SyncResponse {
        server_time: now,
        results,
        scopes,
    }))
}
