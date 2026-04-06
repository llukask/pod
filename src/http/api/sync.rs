use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use crate::{
    app::{decode_sync_cursor, encode_sync_cursor},
    http::{
        auth::ApiUser,
        errors::JsonAppError,
        AppState,
    },
    model::ProgressSyncResponse,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/head", get(sync_head))
        .route("/changes", get(sync_changes))
        .route("/progress", get(sync_progress))
}

/// Returns the current sync head cursor. Clients call this *before*
/// downloading a snapshot so they can later catch up via `/changes`
/// without missing episodes created during the snapshot download.
async fn sync_head(
    user: ApiUser,
    State(state): State<AppState>,
) -> Result<Json<SyncHeadResponse>, JsonAppError> {
    let latest_seq = state
        .app
        .get_latest_seq_for_user(&user.username)
        .await?
        .unwrap_or(0);

    Ok(Json(SyncHeadResponse {
        since: encode_sync_cursor(latest_seq),
    }))
}

#[derive(serde::Serialize)]
struct SyncHeadResponse {
    since: String,
}

#[derive(Deserialize)]
struct SyncParams {
    since: Option<String>,
    limit: Option<i64>,
}

async fn sync_changes(
    user: ApiUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<SyncParams>,
) -> Result<impl IntoResponse, JsonAppError> {
    let limit = params.limit.unwrap_or(200).clamp(1, 2000);

    let since_seq = match &params.since {
        Some(cursor) => decode_sync_cursor(cursor)?,
        None => 0,
    };

    // ETag / If-None-Match: avoid serializing the full response when the
    // client already has the latest state.
    let latest_seq = state
        .app
        .get_latest_seq_for_user(&user.username)
        .await?;
    let etag = format!(
        "\"sync:{}:s{}\"",
        user.username,
        latest_seq.unwrap_or(0)
    );

    if let Some(inm) = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) {
        if inm == etag {
            return Ok((StatusCode::NOT_MODIFIED, HeaderMap::new(), String::new()).into_response());
        }
    }

    // TODO: return 410 Gone when the cursor is too old (expired / pruned
    // from the change log) so the client knows to perform a full resync.

    let response = state
        .app
        .get_sync_changes(&user.username, since_seq, limit)
        .await?;

    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(header::ETAG, etag.parse().expect("valid header value"));
    resp_headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("valid header value"),
    );

    Ok((StatusCode::OK, resp_headers, Json(response)).into_response())
}

// ==============================================================================
// Progress sync
// ==============================================================================

#[derive(Deserialize)]
struct ProgressSyncParams {
    /// RFC 3339 timestamp; returns progress changes strictly after this time.
    since: Option<chrono::DateTime<chrono::Utc>>,
}

/// Returns all progress updates for the authenticated user's episodes that
/// changed after the given timestamp. The client should store the returned
/// `server_time` and pass it as `since` on the next call.
async fn sync_progress(
    user: ApiUser,
    State(state): State<AppState>,
    Query(params): Query<ProgressSyncParams>,
) -> Result<Json<ProgressSyncResponse>, JsonAppError> {
    let since = params.since.unwrap_or(chrono::DateTime::UNIX_EPOCH);

    let response = state
        .app
        .get_progress_changes(&user.username, since)
        .await?;

    Ok(Json(response))
}
