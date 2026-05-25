use std::path::Path;
use std::sync::OnceLock;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

static LOG_PATH: OnceLock<String> = OnceLock::new();

pub fn log_path() -> &'static str {
    LOG_PATH.get().map(|s| s.as_str()).unwrap_or("quill.log")
}

pub fn init(app_data_dir: &Path) {
    let log_dir = app_data_dir.to_path_buf();
    std::fs::create_dir_all(&log_dir).ok();

    let log_path = log_dir.join("quill.log");
    let _ = LOG_PATH.set(log_path.display().to_string());

    let file_appender =
        tracing_appender::rolling::daily(&log_dir, "quill.log");

    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    std::mem::forget(_guard);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(non_blocking);

    let stderr_layer = tracing_subscriber::fmt::layer().with_ansi(true);

    let filter = EnvFilter::try_from_env("QUILL_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .init();
}
