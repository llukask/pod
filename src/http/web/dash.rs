use askama::Template;
use askama_axum::IntoResponse;
use axum::extract::State;

use crate::model::PodcastWithEpisodeStats;

use crate::http::{auth::UserProfile, errors::ApiError, AppState};

#[derive(Template)]
#[template(path = "dash.html")]
struct DashboardTemplate {
    #[allow(dead_code)]
    user: UserProfile,
    subscribed: Vec<PodcastWithEpisodeStats>,
}

pub async fn dash(
    State(state): State<AppState>,
    user: UserProfile,
) -> Result<impl IntoResponse, ApiError> {
    let subscribed = state.app.get_podcasts_for_user(&user.email).await?;

    let t = DashboardTemplate { user, subscribed };
    Ok(t.into_response())
}
