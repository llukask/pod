use askama_axum::IntoResponse;
use axum::{extract::State, response::Redirect, Form};
use serde::Deserialize;

use crate::http::{auth::UserProfile, errors::ApiError, AppState};

#[derive(Deserialize)]
pub struct AddFeedRequest {
    pub feed_url: String,
}

pub async fn add_feed(
    user: UserProfile,
    State(state): State<AppState>,
    Form(req): Form<AddFeedRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let podcast = state.app.add_podcast(&req.feed_url).await?;
    state
        .app
        .subscribe_to_podcast(&user.email, &podcast.id)
        .await?;
    Ok(Redirect::to("/dash"))
}
