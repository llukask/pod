use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use crate::{
    app::decode_sync_cursor,
    http::{
        auth::ApiUser,
        errors::JsonAppError,
        AppState,
    },
};

pub fn router() -> Router<AppState> {
    Router::new().route("/changes", get(sync_changes))
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
