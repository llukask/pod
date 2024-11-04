use askama::Template;
use askama_axum::IntoResponse;
use axum::extract::State;

use crate::db::PodcastWithEpisodeStats;

use crate::http::{auth::UserProfile, errors::ApiError, AppState};

#[derive(Template)]
#[template(path = "dash.html")]
struct DashboardTemplate {
    user: UserProfile,
    subscribed: Vec<PodcastWithEpisodeStats>,
}

pub async fn dash(
    State(state): State<AppState>,
    user: UserProfile,
) -> Result<impl IntoResponse, ApiError> {
    let subscribed = state.db.get_subscribed_feeds_for_user(&user.email).await?;

    let t = DashboardTemplate { user, subscribed };
    Ok(t.into_response())
}
