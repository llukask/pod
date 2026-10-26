use std::io::BufRead;
use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use color_eyre::eyre::{Result, WrapErr};
use pod_protocol::{EpisodeId, Timestamp};
use pod_server::config::Config;
use pod_server::fetch::{FetchPolicy, Fetcher};
use pod_server::poller::Poller;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

/// Pod sync server. Without a command it serves the HTTP API and polls
/// feeds.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Operator commands. They use the configured database and exit
    /// without serving.
    #[command(subcommand)]
    Admin(AdminCommand),
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Tombstone an episode so clients drop it on their next sync.
    PurgeEpisode { episode_id: EpisodeId },
    /// List accounts with their number of live sessions.
    ListUsers,
    /// Set a user's password, read as one line from stdin, and sign them
    /// out on every device.
    ResetPassword { username: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install().context("install the color-eyre report handler")?;
    let cli = Cli::parse();
    // An invalid RUST_LOG is an error rather than a silent fallback to the
    // default level, so a typo doesn't hide the logs someone asked for.
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()
        .context("parse RUST_LOG")?;
    // Admin commands print their result on stdout, so their logs go to
    // stderr where they don't mix into it.
    let logs = tracing_subscriber::fmt().with_env_filter(filter);
    match cli.command {
        None => logs.init(),
        Some(_) => logs.with_writer(std::io::stderr).init(),
    }

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

    match cli.command {
        None => serve(config, pool).await,
        Some(Command::Admin(command)) => admin(command, &pool).await,
    }
}

async fn admin(command: AdminCommand, pool: &sqlx::PgPool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    match command {
        AdminCommand::PurgeEpisode { episode_id } => {
            pod_server::admin::purge_episode(pool, episode_id, &mut out).await
        }
        AdminCommand::ListUsers => {
            pod_server::admin::list_users(pool, Timestamp::now(), &mut out).await
        }
        AdminCommand::ResetPassword { username } => {
            // stdin rather than an argument keeps the password out of shell
            // history and the process list. Only the line ending is
            // stripped; spaces belong to the password.
            let mut password = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut password)
                .context("read the new password from stdin")?;
            let password = password.strip_suffix('\n').unwrap_or(&password);
            let password = password.strip_suffix('\r').unwrap_or(password);
            pod_server::admin::reset_password(pool, &username, password.to_owned(), &mut out).await
        }
    }
}

/// Serves the HTTP API, with the feed poller and the daily housekeeping job
/// running beside it, until SIGINT or SIGTERM.
async fn serve(config: Config, pool: sqlx::PgPool) -> Result<()> {
    // Signal handlers are installed before binding so a failure surfaces at
    // startup rather than on the first shutdown request. A terminal sends
    // SIGINT and systemd stops the service with SIGTERM. Unix-only, which
    // matches every deploy target.
    let mut interrupt = signal(SignalKind::interrupt()).context("install the SIGINT handler")?;
    let mut terminate = signal(SignalKind::terminate()).context("install the SIGTERM handler")?;
    let background_shutdown = CancellationToken::new();
    let stop_background = background_shutdown.clone();
    let housekeeping = tokio::spawn(pod_server::housekeeping::run(
        pool.clone(),
        background_shutdown.clone(),
    ));
    let shutdown = async move {
        tokio::select! {
            _ = interrupt.recv() => tracing::info!("received SIGINT, shutting down"),
            _ = terminate.recv() => tracing::info!("received SIGTERM, shutting down"),
        }
        stop_background.cancel();
    };

    // Shutdown abandons polls in flight rather than waiting for slow feed
    // hosts; their leases make those feeds due again (spec 3.1).
    let fetcher = Fetcher::new(
        FetchPolicy::new(config.fetch_allowlist.clone()),
        &config.base_url,
    )
    .context("build the feed fetcher")?;
    let mut poller = Poller::new(pool.clone(), fetcher, config.poll_interval)
        .spawn(config.poller_workers, background_shutdown);

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
    housekeeping.await.context("join the housekeeping job")?;
    Ok(())
}
