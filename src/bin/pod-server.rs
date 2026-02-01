use std::sync::Arc;

use anyhow::Result;
use axum::{
    response::{IntoResponse, Redirect},
    routing::{get, post},
    Router,
};
use axum_extra::extract::cookie::Key;
use base64::{prelude::BASE64_STANDARD, Engine as _};
use dotenv::dotenv;
use pod::{
    app::App,
    config::Config,
    db::Db,
    http::{
        auth::{self, MaybeUser},
        web::*,
        AppState,
    },
};
use rand::RngCore;
use reqwest::Client as ReqwestClient;
use sqlx::PgPool;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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
    let db: Arc<Db> = pod::db::Db::init(db).await?.into();

    let key = if let Some(ref key) = config.cookie_key {
        let key_bytes = BASE64_STANDARD
            .decode(key.as_bytes())
            .expect("invalid cookie key");
        Key::from(&key_bytes)
    } else {
        let mut key_bytes: [u8; 64] = [0; 64];
        rand::thread_rng().fill_bytes(&mut key_bytes);

        let b64_encoded = BASE64_STANDARD.encode(key_bytes);
        info!("generated new key: \"{}\"", b64_encoded);

        Key::from(&key_bytes)
    };

    let app = Arc::new(App::new(db.clone(), http.clone()));
    let state = AppState {
        db: db.clone(),
        http: http.clone(),
        app: app.clone(),
        key,
        base_url: config.base_url,
        cookie_domain: config.cookie_domain,
        allow_registration: config.allow_registration,
    };

    let router = Router::new()
        .route("/assets/main.css", get(main_css))
        .nest("/auth", auth::router())
        .route("/dash", get(dash))
        .route("/podcast/:podcast_id", get(podcast))
        .route("/add_feed", post(add_feed))
        .route("/report_progress", post(report_progress))
        .route("/", get(index))
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

async fn index(maybe_user: MaybeUser) -> impl IntoResponse {
    match maybe_user {
        MaybeUser::LoggedIn(_) => Redirect::to("/dash"),
        MaybeUser::LoggedOut => Redirect::to("/auth/login"),
    }
}

async fn main_css() -> impl IntoResponse {
    let body = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/main.css"));

    let mut headers = axum::http::header::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "text/css".parse().unwrap(),
    );

    (headers, body)
}
