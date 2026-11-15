//! Log files in `%LOCALAPPDATA%\clipr\logs\`, one per day, the last 7 kept.
//!
//! The level comes from `RUST_LOG` when set (for example `RUST_LOG=debug`), else `info`. Debug builds also
//! log to the console.

use std::path::Path;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Keep the returned guard alive for the whole run: dropping it flushes and stops the log writer.
pub fn init(log_dir: &Path) -> Result<WorkerGuard> {
    std::fs::create_dir_all(log_dir).with_context(|| format!("can't create {}", log_dir.display()))?;
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("clipr")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir)
        .context("can't open the log file")?;
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false);
    let console_layer = cfg!(debug_assertions).then(tracing_subscriber::fmt::layer);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(console_layer)
        .try_init()
        .context("logging was already set up")?;
    Ok(guard)
}
