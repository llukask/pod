use axum::{extract::State, http, response::IntoResponse, Json};
use serde::Deserialize;

use crate::http::{auth::UserProfile, errors::ApiError, AppState};

#[derive(Deserialize)]
pub struct ProgressReport {
    episode_id: String,
    progress: i32,
    done: bool,
}

pub async fn report_progress(
    user: UserProfile,
    State(state): State<AppState>,
    Json(report): Json<ProgressReport>,
) -> Result<impl IntoResponse, ApiError> {
    let episode_id = report.episode_id;
    let progress = report.progress;
    let done = report.done;

    state
        .db
        .update_progress(&user.email, &episode_id, progress, done)
        .await?;

    Ok(http::StatusCode::OK)
}
