use askama::Template;
use axum::extract::{Path, State};
use axum::response::IntoResponse;

use crate::db::{EpisodeWithProgress, Podcast};

use crate::http::errors::ApiError;
use crate::http::{auth::UserProfile, AppState};

#[derive(Template)]
#[template(path = "podcast.html")]
struct PodcastTemplate {
    podcast: Podcast,
    episodes: Vec<EpisodeWithProgress>,
}

pub fn has_html_tags(s: &str) -> bool {
    (s.contains("<p") && s.contains("</p>")) || (s.contains("<div>") && s.contains("</div>"))
}

pub fn split_paragraphs(s: &str) -> Vec<String> {
    s.split("\n\n").map(|p| p.to_string()).collect()
}

pub async fn podcast(
    user: UserProfile,
    State(state): State<AppState>,
    Path(podcast_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let podcast = state.db.get_podcast_by_id(&podcast_id).await?;
    if let Some(podcast) = podcast {
        let mut episodes = state
            .db
            .get_episodes_with_progress_for_podcast(&user.email, &podcast.id)
            .await?;
        episodes.sort_by(|a, b| b.episode.publication_date.cmp(&a.episode.publication_date));

        let t = PodcastTemplate { podcast, episodes };
        Ok(t.into_response())
    } else {
        Err(ApiError::NotFound("podcast".to_string(), podcast_id))
    }
}
