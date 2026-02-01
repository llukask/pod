use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use crate::{
    http::{
        auth::ApiUser,
        errors::{AppError, JsonAppError},
        AppState,
    },
    model::{EpisodeWithProgress, Podcast, PodcastWithEpisodeStats},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_podcasts).post(add_podcast))
        .route("/:id", get(get_podcast))
        .route("/:id/episodes", get(list_episodes))
}

async fn list_podcasts(
    user: ApiUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<PodcastWithEpisodeStats>>, JsonAppError> {
    let podcasts = state.app.get_podcasts_for_user(&user.username).await?;
    Ok(Json(podcasts))
}

async fn get_podcast(
    _user: ApiUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Podcast>, JsonAppError> {
    let podcast = state
        .app
        .get_podcast(&id)
        .await?
        .ok_or_else(|| AppError::NotFound("podcast".to_string(), id))?;
    Ok(Json(podcast))
}

#[derive(Deserialize)]
struct AddPodcastRequest {
    feed_url: String,
}

async fn add_podcast(
    user: ApiUser,
    State(state): State<AppState>,
    Json(req): Json<AddPodcastRequest>,
) -> Result<Json<Podcast>, JsonAppError> {
    let podcast = state.app.add_podcast(&req.feed_url).await?;
    state
        .app
        .subscribe_to_podcast(&user.username, &podcast.id)
        .await?;
    Ok(Json(podcast))
}

async fn list_episodes(
    user: ApiUser,
    State(state): State<AppState>,
    Path(podcast_id): Path<String>,
) -> Result<Json<Vec<EpisodeWithProgress>>, JsonAppError> {
    let episodes = state
        .app
        .get_episodes_with_progress(&user.username, &podcast_id)
        .await?;
    Ok(Json(episodes))
}
