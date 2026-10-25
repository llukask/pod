//! Logs go to stderr and to a daily file under the platform data directory,
//! so a user can attach the file to a bug report.

use std::path::Path;

use color_eyre::eyre::{Result, WrapErr};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// A week of daily files is enough to cover "it broke a few days ago".
const KEPT_LOG_FILES: usize = 7;

/// Installs the global subscriber. Creates `log_dir`, so call it before the
/// UI starts. The file writer runs on its own thread; keep the returned guard
/// alive until exit so it flushes.
pub fn init(log_dir: &Path) -> Result<WorkerGuard> {
    std::fs::create_dir_all(log_dir)
        .with_context(|| format!("create the log directory {}", log_dir.display()))?;

    // An invalid RUST_LOG is an error rather than a silent fallback to the
    // default level, as in pod-server.
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()
        .context("parse RUST_LOG")?;

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("pod-desktop")
        .filename_suffix("log")
        .max_log_files(KEPT_LOG_FILES)
        .build(log_dir)
        .context("open the log file")?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .try_init()
        .context("install the tracing subscriber")?;
    Ok(guard)
}
