use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::State,
    http::header::{self, HeaderMap},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
    Extension, Router,
};
use axum_extra::extract::cookie::Key;
use base64::{prelude::BASE64_STANDARD, Engine as _};
use dotenv::dotenv;
use pod::{
    app::App,
    db::Db,
    http::{
        auth::{self, build_google_oauth_client, MaybeUser},
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

    let oauth_id = std::env::var("GOOGLE_OAUTH_CLIENT_ID")?;
    let oauth_secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET")?;
    let base_url = std::env::var("BASE_URL")?;
    let cookie_domain = std::env::var("COOKIE_DOMAIN")?;
    let cookie_key_base64: Option<String> = std::env::var("COOKIE_KEY").ok();

    let http = ReqwestClient::new();
    let db = PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
    let db: Arc<Db> = pod::db::Db::init(db).await?.into();

    let key = if let Some(key) = cookie_key_base64 {
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

    let client = build_google_oauth_client(oauth_id.clone(), oauth_secret, &base_url);

    let app = Arc::new(App::new(db.clone(), http.clone()));
    let state = AppState {
        db: db.clone(),
        http: http.clone(),
        app: app.clone(),
        key,
        base_url,
        cookie_domain,
    };

    let router = Router::new()
        .route("/assets/main.css", get(main_css))
        .nest("/auth", auth::router())
        .route("/dash", get(dash))
        .route("/podcast/:podcast_id", get(podcast))
        .route("/add_feed", post(add_feed))
        .route("/report_progress", post(report_progress))
        .route("/", get(index))
        .layer(Extension(client))
        .layer(Extension(oauth_id))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let jh = tokio::spawn(async move {
        let app = app.clone();

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(600));
        loop {
            match app.refresh_all_podcasts().await {
                Ok(_) => {}
                Err(e) => warn!("error refreshing podcasts: {:?}", e),
            }
            interval.tick().await;
        }
    });

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, router).await?;

    jh.await?;

    Ok(())
}

#[axum::debug_handler]
async fn index(
    maybe_user: MaybeUser,
    State(state): State<AppState>,
    Extension(oauth_id): Extension<String>,
) -> impl IntoResponse {
    match maybe_user {
        MaybeUser::LoggedIn(_) => Redirect::to("/dash").into_response(),
        MaybeUser::LoggedOut => {
            Html(format!("<p>Please Log In!</p>

                <a href=\"https://accounts.google.com/o/oauth2/v2/auth?scope=openid%20email&client_id={oauth_id}&response_type=code&redirect_uri={base_url}/auth/google_callback\">
                Click here to sign into Google!
                </a>", base_url = state.base_url)).into_response()
        }
    }
}

async fn main_css() -> impl IntoResponse {
    let body = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/main.css"));

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/css".parse().unwrap());

    (headers, body)
}
