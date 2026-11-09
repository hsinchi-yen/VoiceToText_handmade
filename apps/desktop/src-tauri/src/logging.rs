//! `tracing` setup: daily-rotated files in `%APPDATA%\v2c\logs\`.
//!
//! Privacy (AGENTS.md rule 1): nothing in the app logs recognised text, polished text, or
//! vocabulary; only lengths, timings, provider names, and error codes. Types carrying user
//! text redact it in `Debug`. `controller::tests::logs_never_contain_the_sentence` checks this
//! with the same formatter as the log file.

use std::path::Path;

use tracing::Subscriber;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt, EnvFilter, Registry};

/// Default filter; override with `RUST_LOG` (e.g. `RUST_LOG=debug`).
const DEFAULT_FILTER: &str = "info,v2c=debug,v2c_core=debug";

/// Builds the subscriber used for the log file, writing to `writer`.
pub fn subscriber<W>(writer: W, filter: EnvFilter) -> impl Subscriber + Send + Sync
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    Registry::default().with(filter).with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_thread_names(true)
            .with_writer(writer),
    )
}

/// Installs the global subscriber. Keep the returned guard alive until exit (it flushes).
pub fn init(logs_dir: &Path) -> Option<WorkerGuard> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    if std::fs::create_dir_all(logs_dir).is_err() {
        // No log directory: fall back to stderr so `tauri dev` still shows something.
        let _ = tracing::subscriber::set_global_default(subscriber(std::io::stderr, filter));
        return None;
    }
    let appender = tracing_appender::rolling::daily(logs_dir, "v2c.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let _ = tracing::subscriber::set_global_default(subscriber(writer, filter));
    Some(guard)
}
