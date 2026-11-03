//! Request logging: where records go, and how much of them is kept.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default = "default_max_disk_mb")]
    pub max_disk_mb: u64,
    #[serde(default = "default_true")]
    pub conversion_log: bool,
    /// Log the headers sent to the upstream provider (sensitive values redacted).
    #[serde(default = "default_false")]
    pub upstream_headers_log: bool,
    #[serde(default)]
    pub console: ConsoleConfig,
    #[serde(default)]
    pub files: Vec<LogFileConfig>,
    #[serde(default)]
    pub request_log: RequestLogConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequestLogConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
    #[serde(default = "default_max_capture_bytes")]
    pub max_capture_bytes: usize,
    #[serde(default = "default_true")]
    pub trace_enabled: bool,
    #[serde(default = "default_trace_preview_bytes")]
    pub trace_max_preview_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_console_output")]
    pub output: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default = "default_console_format")]
    pub format: String,
}

pub(super) fn default_console_output() -> String {
    "stdout".to_string()
}

pub(super) fn default_console_format() -> String {
    "pretty".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogFileConfig {
    pub name: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "default_file_format")]
    pub format: String,
    #[serde(default = "default_rotation")]
    pub rotation: String,
}

pub(super) fn default_file_format() -> String {
    "json".to_string()
}

pub(super) fn default_rotation() -> String {
    "daily".to_string()
}

pub(super) fn default_log_level() -> String {
    "info".to_string()
}

pub(super) fn default_max_disk_mb() -> u64 {
    500
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_false() -> bool {
    false
}

pub(super) fn default_max_capture_bytes() -> usize {
    10 * 1024 * 1024
}

pub(super) fn default_trace_preview_bytes() -> usize {
    2048
}
