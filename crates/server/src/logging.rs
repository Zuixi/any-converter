//! Runtime logging shared by CLI and Desktop entrypoints.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config::LoggingConfig;
use chrono::Local;
use log::{LevelFilter, Log, Metadata, Record};

// ── Public API ───────────────────────────────────────────────────────

/// Initialize the global logger from config.
///
/// Builds a `MultiLogger` with console + file outputs based on
/// `LoggingConfig` and installs it via `log::set_boxed_logger`.
///
/// Returns the resolved log directory (if any) so the caller can
/// spawn a disk-quota manager.
pub fn init_logging(config: &LoggingConfig) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let global_level = parse_level_filter(&config.level);
    let mut outputs: Vec<Box<dyn Output + Send + Sync>> = Vec::new();

    // Console output
    if config.console.enabled {
        let level = config
            .console
            .level
            .as_deref()
            .map(parse_level_filter)
            .unwrap_or(global_level);
        let format = parse_log_format(&config.console.format);
        let is_stderr = config.console.output == "stderr";
        outputs.push(Box::new(ConsoleOutput {
            level,
            format,
            is_stderr,
        }));
    }

    // Resolve log directory
    let log_dir = match &config.dir {
        Some(dir) => {
            let expanded = expand_tilde(dir);
            let path = PathBuf::from(expanded);
            if let Err(e) = std::fs::create_dir_all(&path) {
                let _ = writeln!(
                    std::io::stderr(),
                    "WARN: failed to create log dir {}: {e}",
                    path.display()
                );
            }
            Some(path)
        }
        None => None,
    };

    // File outputs (only when dir is configured)
    if let Some(ref dir) = log_dir {
        for file_cfg in &config.files {
            let level = file_cfg
                .level
                .as_deref()
                .map(parse_level_filter)
                .unwrap_or(global_level);
            let format = parse_log_format(&file_cfg.format);
            let rotation = parse_rotation(&file_cfg.rotation);
            let target_filter = file_cfg.target.clone();

            let writer = RotatingWriter::new(dir, &file_cfg.name, rotation)?;
            outputs.push(Box::new(FileOutput {
                level,
                format,
                target_filter,
                writer: Mutex::new(writer),
            }));
        }
    }

    let logger = MultiLogger {
        max_level: global_level,
        outputs,
    };

    log::set_boxed_logger(Box::new(logger)).map_err(|e| format!("failed to set logger: {e}"))?;
    log::set_max_level(global_level);

    Ok(log_dir)
}

// ── MultiLogger ──────────────────────────────────────────────────────

struct MultiLogger {
    max_level: LevelFilter,
    outputs: Vec<Box<dyn Output + Send + Sync>>,
}

impl Log for MultiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.max_level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        for output in &self.outputs {
            if output.enabled(record.metadata()) {
                output.write(record);
            }
        }
    }

    fn flush(&self) {
        for output in &self.outputs {
            output.flush();
        }
    }
}

// ── Output trait ─────────────────────────────────────────────────────

trait Output: Send + Sync {
    fn enabled(&self, metadata: &Metadata) -> bool;
    fn write(&self, record: &Record);
    fn flush(&self);
}

// ── ConsoleOutput ────────────────────────────────────────────────────

struct ConsoleOutput {
    level: LevelFilter,
    format: LogFormat,
    is_stderr: bool,
}

impl Output for ConsoleOutput {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn write(&self, record: &Record) {
        let line = match self.format {
            LogFormat::Pretty => format_pretty(record),
            LogFormat::Json => format_json(record),
        };
        if self.is_stderr {
            let _ = writeln!(std::io::stderr(), "{line}");
        } else {
            #[allow(clippy::print_stdout)]
            {
                let _ = writeln!(std::io::stdout(), "{line}");
            }
        }
    }

    fn flush(&self) {
        if self.is_stderr {
            let _ = std::io::stderr().flush();
        } else {
            let _ = std::io::stdout().flush();
        }
    }
}

// ── FileOutput ───────────────────────────────────────────────────────

struct FileOutput {
    level: LevelFilter,
    format: LogFormat,
    target_filter: Option<String>,
    writer: Mutex<RotatingWriter>,
}

impl Output for FileOutput {
    fn enabled(&self, metadata: &Metadata) -> bool {
        if metadata.level() > self.level {
            return false;
        }
        if let Some(ref target) = self.target_filter {
            return metadata.target() == target;
        }
        true
    }

    fn write(&self, record: &Record) {
        let line = match self.format {
            LogFormat::Pretty => format_pretty(record),
            LogFormat::Json => format_json(record),
        };
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.maybe_rotate();
            let _ = writeln!(w.writer, "{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.writer.flush();
        }
    }
}

// ── RotatingWriter ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rotation {
    Daily,
    Never,
}

struct RotatingWriter {
    dir: PathBuf,
    prefix: String,
    current_date: String,
    writer: BufWriter<File>,
    rotation: Rotation,
}

impl RotatingWriter {
    fn new(
        dir: &Path,
        prefix: &str,
        rotation: Rotation,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let current_date = today_str();
        let path = Self::build_path(dir, prefix, &current_date, rotation);
        let file = open_private_append(&path)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
            current_date,
            writer: BufWriter::new(file),
            rotation,
        })
    }

    fn maybe_rotate(&mut self) -> Result<(), std::io::Error> {
        if self.rotation == Rotation::Never {
            return Ok(());
        }
        let now = today_str();
        if now == self.current_date {
            return Ok(());
        }
        let _ = self.writer.flush();
        let path = Self::build_path(&self.dir, &self.prefix, &now, self.rotation);
        let file = open_private_append(&path)?;
        self.writer = BufWriter::new(file);
        self.current_date = now;
        Ok(())
    }

    fn build_path(dir: &Path, prefix: &str, date: &str, rotation: Rotation) -> PathBuf {
        match rotation {
            Rotation::Daily => dir.join(format!("{prefix}.{date}.jsonl")),
            Rotation::Never => dir.join(format!("{prefix}.jsonl")),
        }
    }
}

/// Open a log file for append, creating it 0600 and repairing the mode of
/// an existing one. Log files can carry a page key or request bodies, so
/// the default umask (typically world-readable 0644) is never acceptable.
pub(crate) fn open_private_append(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        repair_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    repair_private_file(path)?;
    Ok(file)
}

pub(crate) fn repair_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn repair_private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

// ── Formatting ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum LogFormat {
    Pretty,
    Json,
}

fn format_pretty(record: &Record) -> String {
    let now = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
    let level = record.level();
    let level_colored = match level {
        log::Level::Error => "\x1b[31mERROR\x1b[0m",
        log::Level::Warn => "\x1b[33mWARN\x1b[0m",
        log::Level::Info => "\x1b[32mINFO\x1b[0m",
        log::Level::Debug => "\x1b[34mDEBUG\x1b[0m",
        log::Level::Trace => "\x1b[35mTRACE\x1b[0m",
    };
    let target = record.target();
    format!("{now} {level_colored} {target}: {}", record.args())
}

fn format_json(record: &Record) -> String {
    let now = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%z");
    let msg = record.args().to_string();
    let msg_escaped = escape_json_string(&msg);
    let target = record.target();
    let target_escaped = escape_json_string(target);
    format!(
        r#"{{"timestamp":"{now}","level":"{}","target":"{target_escaped}","message":"{msg_escaped}"}}"#,
        record.level()
    )
}

fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_level_filter(s: &str) -> LevelFilter {
    match s.to_lowercase().as_str() {
        "off" => LevelFilter::Off,
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}

fn parse_log_format(s: &str) -> LogFormat {
    match s.to_lowercase().as_str() {
        "json" => LogFormat::Json,
        _ => LogFormat::Pretty,
    }
}

fn parse_rotation(s: &str) -> Rotation {
    match s.to_lowercase().as_str() {
        "never" | "none" => Rotation::Never,
        _ => Rotation::Daily,
    }
}

fn today_str() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{rest}", home.to_string_lossy());
        }
    }
    path.to_string()
}

#[cfg(test)]
mod permission_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_append_repairs_file_and_parent_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap().path().join("logs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("app.jsonl");
        std::fs::write(&path, b"old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        drop(open_private_append(&path).unwrap());

        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
