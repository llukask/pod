use std::net::SocketAddr;

use color_eyre::eyre::{Result, WrapErr};
use pod_server::config::Config;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install().context("install the color-eyre report handler")?;
    // An invalid RUST_LOG is an error rather than a silent fallback to the
    // default level, so a typo doesn't hide the logs someone asked for.
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()
        .context("parse RUST_LOG")?;
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Config::load().context("load the server config")?;

    // The schema must be current before anything reads or writes it, so a
    // failed migration stops the server instead of serving errors.
    // TODO: hand the pool to the poller once it exists.
    let pool = pod_server::db::connect(&config.database_url)
        .await
        .context("open the database pool")?;
    pod_server::db::migrate(&pool)
        .await
        .context("bring the database schema up to date")?;
    tracing::info!("database schema is up to date");

    // Signal handlers are installed before binding so a failure surfaces at
    // startup rather than on the first shutdown request. Fly.io stops
    // machines with SIGINT, systemd and Docker use SIGTERM. Unix-only, which
    // matches every deploy target.
    let mut interrupt = signal(SignalKind::interrupt()).context("install the SIGINT handler")?;
    let mut terminate = signal(SignalKind::terminate()).context("install the SIGTERM handler")?;
    let shutdown = async move {
        tokio::select! {
            _ = interrupt.recv() => tracing::info!("received SIGINT, shutting down"),
            _ = terminate.recv() => tracing::info!("received SIGTERM, shutting down"),
        }
    };

    let listener = TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("bind {}", config.listen_addr))?;
    tracing::info!(listen_addr = %config.listen_addr, "listening");
    // Connect info gives handlers the peer address, which the auth rate
    // limit keys on.
    let app = pod_server::app(pod_server::AppState::new(pool, &config));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .context("serve HTTP")?;
    Ok(())
}
