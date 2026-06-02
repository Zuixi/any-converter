use std::path::{Path, PathBuf};

use any_converter_server::config::{LogFileConfig, LoggingConfig};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

/// Initialize tracing with multi-layer subscriber.
///
/// Layers:
/// - Console: human-readable output (always present)
/// - General file: JSON, non-blocking, daily rolling
/// - Error file: JSON, non-blocking, daily rolling, ERROR-only
/// - Conversion file: JSON, non-blocking, target="conversion"
/// - Custom files from config
///
/// Returns guards that must be held for the application lifetime;
/// dropping them flushes pending log writes.
pub fn init_tracing(
    config: &LoggingConfig,
) -> Result<(Vec<WorkerGuard>, Option<PathBuf>), Box<dyn std::error::Error>> {
    let mut guards: Vec<WorkerGuard> = Vec::new();

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.level));

    // Console layer — human-readable, colored
    let console_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_filter(env_filter);

    let log_dir = match &config.dir {
        Some(dir) => {
            let expanded = expand_tilde(dir);
            let path = PathBuf::from(expanded);
            if let Err(e) = std::fs::create_dir_all(&path) {
                tracing::warn!("failed to create log dir {}: {e}", path.display());
            }
            Some(path)
        }
        None => None,
    };

    if let Some(ref dir) = log_dir {
        let (general_layer, guard) = make_json_file_layer(dir, "general", &config.level)?;
        guards.push(guard);

        let (error_layer, guard) = make_json_file_layer(dir, "error", "error")?;
        guards.push(guard);

        let (conversion_layer, guard) =
            make_target_file_layer(dir, "conversion", "debug", "conversion")?;
        guards.push(guard);

        let mut custom_layers: Vec<Box<dyn Layer<_> + Send + Sync>> = Vec::new();
        for file_cfg in &config.files {
            if matches!(file_cfg.name.as_str(), "general" | "error" | "conversion") {
                continue;
            }
            let (layer, guard) = make_custom_file_layer(dir, file_cfg)?;
            guards.push(guard);
            custom_layers.push(layer);
        }

        let registry = tracing_subscriber::registry()
            .with(console_layer)
            .with(general_layer)
            .with(error_layer)
            .with(conversion_layer);

        // Dynamic custom layers — fold them via Option chaining since
        // tracing_subscriber requires static layer types.
        // For simplicity, apply them as boxed layers.
        if custom_layers.is_empty() {
            registry.init();
        } else {
            let mut composed: Box<dyn Layer<_> + Send + Sync> = custom_layers.remove(0);
            for layer in custom_layers {
                composed = Box::new(composed.and_then(layer));
            }
            registry.with(composed).init();
        }
    } else {
        tracing_subscriber::registry().with(console_layer).init();
    }

    Ok((guards, log_dir))
}

fn make_json_file_layer<S>(
    dir: &Path,
    prefix: &str,
    level: &str,
) -> Result<
    (
        Box<dyn Layer<S> + Send + Sync>,
        WorkerGuard,
    ),
    Box<dyn std::error::Error>,
>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let file_appender = tracing_appender::rolling::daily(dir, format!("{prefix}.jsonl"));
    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::new(level);
    let layer = fmt::layer()
        .json()
        .with_writer(writer)
        .with_target(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_filter(filter);

    Ok((Box::new(layer), guard))
}

fn make_target_file_layer<S>(
    dir: &Path,
    prefix: &str,
    level: &str,
    target: &str,
) -> Result<
    (
        Box<dyn Layer<S> + Send + Sync>,
        WorkerGuard,
    ),
    Box<dyn std::error::Error>,
>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let file_appender = tracing_appender::rolling::daily(dir, format!("{prefix}.jsonl"));
    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter_str = format!("{target}={level}");
    let filter = EnvFilter::new(filter_str);
    let layer = fmt::layer()
        .json()
        .with_writer(writer)
        .with_target(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_filter(filter);

    Ok((Box::new(layer), guard))
}

fn make_custom_file_layer<S>(
    dir: &Path,
    cfg: &LogFileConfig,
) -> Result<
    (
        Box<dyn Layer<S> + Send + Sync>,
        WorkerGuard,
    ),
    Box<dyn std::error::Error>,
>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let level = cfg.level.as_deref().unwrap_or("info");
    match &cfg.target {
        Some(target) => make_target_file_layer(dir, &cfg.name, level, target),
        None => make_json_file_layer(dir, &cfg.name, level),
    }
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{rest}", home.to_string_lossy());
        }
    }
    path.to_string()
}
