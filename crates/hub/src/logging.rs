//! tracing to stderr + `data_dir/hub.log`.

use std::io::IsTerminal;
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Keep alive for the life of the process so buffered file logs get flushed.
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

fn filter(default: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default))
}

fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Simple size cap: hub.log -> hub.log.1 once it grows past 10 MB.
    if std::fs::metadata(path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let old = path.with_extension("log.1");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(path, &old);
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)
}

/// `RUST_LOG` overrides both defaults. `log_file` None = stderr only.
pub fn init(log_file: Option<&Path>, stderr_default: &str, file_default: &str) -> LogGuard {
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false)
        .with_filter(filter(stderr_default));
    let (file_layer, guard) = match log_file.map(open_log) {
        Some(Ok(file)) => {
            let (writer, guard) = tracing_appender::non_blocking(file);
            let layer = fmt::layer().with_writer(writer).with_ansi(false).with_filter(filter(file_default));
            (Some(layer), Some(guard))
        }
        Some(Err(e)) => {
            eprintln!("krypto: could not open log file: {e}");
            (None, None)
        }
        None => (None, None),
    };
    let _ = tracing_subscriber::registry().with(stderr_layer).with(file_layer).try_init();
    LogGuard { _file: guard }
}
