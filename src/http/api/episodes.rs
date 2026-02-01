use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde::Deserialize;

use crate::http::{auth::ApiUser, errors::JsonAppError, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/:id/progress", post(report_progress))
}

#[derive(Deserialize)]
struct ProgressReport {
    progress: i32,
    done: bool,
}

async fn report_progress(
    user: ApiUser,
    State(state): State<AppState>,
    Path(episode_id): Path<String>,
    Json(report): Json<ProgressReport>,
) -> Result<StatusCode, JsonAppError> {
    state
        .app
        .update_episode_progress(&user.username, &episode_id, report.progress, report.done)
        .await?;
    Ok(StatusCode::OK)
}
