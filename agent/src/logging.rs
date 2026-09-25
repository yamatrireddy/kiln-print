//! Structured logging: human-readable console output plus daily-rotated JSON files.
//!
//! Log targets: `kiln::jobs` (job transitions), `kiln::discovery`, `kiln::monitor`,
//! `kiln::queue`, `kiln::api`, `kiln::audit`, `kiln::windows`. Payloads and document
//! contents are never logged; job names are omitted because they may contain personal data.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

use crate::config::LoggingConfig;

/// Installs the global subscriber. Keep the returned guard alive to flush file logs.
pub fn init(config: &LoggingConfig, data_dir: &Path) -> anyhow::Result<Option<WorkerGuard>> {
    let filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.level));
    let console = config
        .console
        .then(|| fmt::layer().with_target(true).with_filter(filter()));
    let (file, guard) = if config.file {
        let dir = data_dir.join("logs");
        std::fs::create_dir_all(&dir)?;
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("agent")
            .filename_suffix("log")
            .max_log_files(14)
            .build(&dir)?;
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let layer = fmt::layer()
            .json()
            .with_current_span(false)
            .with_writer(writer)
            .with_filter(filter());
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };
    tracing_subscriber::registry()
        .with(console)
        .with(file)
        .try_init()?;
    Ok(guard)
}
