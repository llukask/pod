use std::sync::Arc;

use anyhow::Result;
use axum::{
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
        Method,
    },
    Router,
};
use dotenv::dotenv;
use pod_server::{app::App, config::Config, db::Db, http::AppState};
use reqwest::Client as ReqwestClient;
use sqlx::PgPool;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use pod_server::http::api;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=debug,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::load()?;

    let http = ReqwestClient::new();
    let db = PgPool::connect(&config.database_url).await?;
    let db: Arc<Db> = pod_server::db::Db::init(db).await?.into();

    let app = Arc::new(App::new(db.clone(), http.clone()));
    let state = AppState {
        db: db.clone(),
        http: http.clone(),
        app: app.clone(),
        allow_registration: config.allow_registration,
    };

    let cors = CorsLayer::new()
        // Mirror the request origin so browser clients can call from their own host.
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE])
        // Allow credentials for authenticated calls.
        .allow_credentials(true);

    // The Leptos SPA is built by Trunk into `frontend/dist`. Serve those
    // assets statically; for any path that isn't a real file (e.g. a deep
    // SPA route like `/podcast/abc`), fall back to `index.html` so the
    // client-side router can take over.
    let frontend_dir = "frontend/dist";
    let frontend_index = format!("{}/index.html", frontend_dir);
    let static_service =
        ServeDir::new(frontend_dir).not_found_service(ServeFile::new(frontend_index));

    let router = Router::new()
        .nest("/api/v1", api::router().layer(cors))
        .fallback_service(static_service)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let refresh_interval_secs = config.refresh_interval_secs;
    let jh = tokio::spawn(async move {
        let app = app.clone();

        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(refresh_interval_secs));
        loop {
            match app.refresh_all_podcasts().await {
                Ok(_) => {}
                Err(e) => warn!("error refreshing podcasts: {:?}", e),
            }
            interval.tick().await;
        }
    });

    let bind_addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, router).await?;

    jh.await?;

    Ok(())
}
