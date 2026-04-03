use axum::Router;

use crate::http::AppState;

mod auth;
mod episodes;
mod podcasts;
mod sync;

pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/podcasts", podcasts::router())
        .nest("/episodes", episodes::router())
        .nest("/sync", sync::router())
}
