use std::net::SocketAddr;

use color_eyre::eyre::{Result, WrapErr};
use pod_server::config::Config;
use pod_server::fetch::{FetchPolicy, Fetcher};
use pod_server::poller::Poller;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
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
    let poller_shutdown = CancellationToken::new();
    let stop_poller = poller_shutdown.clone();
    let shutdown = async move {
        tokio::select! {
            _ = interrupt.recv() => tracing::info!("received SIGINT, shutting down"),
            _ = terminate.recv() => tracing::info!("received SIGTERM, shutting down"),
        }
        stop_poller.cancel();
    };

    // Shutdown abandons polls in flight rather than waiting for slow feed
    // hosts; their leases make those feeds due again (spec 3.1).
    let fetcher = Fetcher::new(
        FetchPolicy::new(config.fetch_allowlist.clone()),
        &config.base_url,
    )
    .context("build the feed fetcher")?;
    let mut poller = Poller::new(pool.clone(), fetcher, config.poll_interval)
        .spawn(config.poller_workers, poller_shutdown);

    let listener = TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("bind {}", config.listen_addr))?;
    tracing::info!(listen_addr = %config.listen_addr, "listening");
    // Connect info gives handlers the peer address, which the auth rate
    // limit keys on.
    let state =
        pod_server::AppState::new(pool, &config).context("set up the shared request state")?;
    let app = pod_server::app(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .context("serve HTTP")?;
    while let Some(worker) = poller.join_next().await {
        worker.context("join a poller worker")?;
    }
    Ok(())
}
