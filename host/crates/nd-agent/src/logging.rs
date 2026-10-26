// SPDX-License-Identifier: GPL-3.0-or-later
//! Tracing setup: daily-rotated file under `<data dir>/logs`, optional stderr.

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Number of daily log files kept.
pub const KEEP_FILES: usize = 7;

pub fn log_dir(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("logs")
}

/// Install the global subscriber. `console` adds a stderr layer. Keep the returned guard
/// alive for the whole process, dropping it flushes and stops the file writer.
/// Returns `None` (stderr only) if the log directory is unusable.
pub fn init(data_dir: &Path, console: bool) -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Create the folder first: the appender prunes old files at build time and complains otherwise.
    let _ = std::fs::create_dir_all(log_dir(data_dir));
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("agent.log")
        .max_log_files(KEEP_FILES)
        .build(log_dir(data_dir));
    let (writer, guard) = match appender {
        Ok(a) => {
            let (w, g) = tracing_appender::non_blocking(a);
            (Some(w), Some(g))
        }
        Err(e) => {
            eprintln!("cannot open log file: {e}");
            (None, None)
        }
    };
    let file_layer = writer.map(|w| tracing_subscriber::fmt::layer().with_ansi(false).with_writer(w));
    let stderr_layer =
        (console || guard.is_none()).then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let _ = tracing_subscriber::registry().with(filter).with(file_layer).with(stderr_layer).try_init();
    guard
}
