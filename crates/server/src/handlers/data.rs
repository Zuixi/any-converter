//! The management data endpoints the UI consumes: request logs, gateway
//! status, and the conversion playground.
//!
//! All three sit behind the management middleware in `router.rs`, like every
//! `/v1/admin/*` route. The shapes match what the shared `ApiClient` parses,
//! for both the browser page session and the desktop's IPC proxy.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;

use any_converter_core::convert::Format;
use axum::Json;
use axum::extract::{Query, State, rejection::JsonRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use log::error;
use serde::Deserialize;

use super::{AppState, data_error_response, safe_convert_request, safe_convert_response};
use crate::request_log::RequestLogRecord;
use crate::storage::{SqliteStorage, StorageError};

/// Default page of logs the UI loads.
const DEFAULT_LOG_LIMIT: u64 = 500;
/// Widest page of logs; a wider reply only costs the UI latency.
const MAX_LOG_LIMIT: u64 = 1000;

// ponytail: one gateway per process, so the first router build is process
// start; a static set once beats threading a start `Instant` through every
// state struct.
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Marks the process start for the status `uptime_seconds`. Called once per
/// router build; the first call wins.
pub(crate) fn note_start_time() {
    STARTED.get_or_init(Instant::now);
}

/// `GET /v1/admin/logs?limit=N` — the request log, newest first.
///
/// SQLite is the store, so it answers first. The JSONL mirror only fills in
/// when the store is absent or empty, same as the Next route this replaces:
/// the files are an operator convenience, not a second source of truth.
pub async fn handle_admin_logs(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let limit = match parse_logs_limit(params.get("limit").map(String::as_str)) {
        Ok(limit) => limit,
        Err(InvalidLimit) => {
            return data_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_limit",
                "limit must be a non-negative integer",
            );
        }
    };
    let Some(dir) = state.config.logging.dir.as_deref() else {
        return data_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "logging_not_configured",
            "logging is not configured",
        );
    };
    match request_logs_from_dir(dir, limit) {
        Ok(records) => Json(serde_json::json!({ "records": records })).into_response(),
        Err(err) => {
            error!("request logs failed dir={dir}: {err}");
            data_error_response(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", err)
        }
    }
}

/// `limit`: default 500, clamped to `1..=1000`; anything that is not a
/// non-negative integer is a 400.
#[derive(Debug, Clone, Copy)]
struct InvalidLimit;

fn parse_logs_limit(value: Option<&str>) -> Result<u64, InvalidLimit> {
    let Some(raw) = value else {
        return Ok(DEFAULT_LOG_LIMIT);
    };
    match raw.trim().parse::<i64>() {
        Ok(parsed) if parsed >= 0 => Ok((parsed as u64).clamp(1, MAX_LOG_LIMIT)),
        _ => Err(InvalidLimit),
    }
}

#[derive(Debug, thiserror::Error)]
enum LogsError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] StorageError),
    #[error("log directory error: {0}")]
    Io(#[from] std::io::Error),
}

fn request_logs_from_dir(dir: &str, limit: u64) -> Result<Vec<RequestLogRecord>, LogsError> {
    let db_path = Path::new(dir).join("any-converter.sqlite3");
    if db_path.exists() {
        match SqliteStorage::open_readonly_in_log_dir(dir)
            .and_then(|storage| storage.recent_request_logs(limit))
        {
            // The store answered; only an empty store falls through to the
            // JSONL mirror.
            Ok(records) if !records.is_empty() => return Ok(records),
            Ok(_) => {}
            // A database that has not served anything yet has no table; the
            // mirror may still have files to show.
            Err(err) if err.is_missing_table() => {}
            Err(err) => return Err(err.into()),
        }
    }
    jsonl_request_logs(dir, limit).map_err(LogsError::from)
}

/// The JSONL fallback: the newest three `requests.*.jsonl` files, each line a
/// `RequestLogRecord`, malformed lines skipped, sorted by `timestamp`
/// descending (string compare, same as the Next route), cut to `limit`.
fn jsonl_request_logs(dir: &str, limit: u64) -> std::io::Result<Vec<RequestLogRecord>> {
    let mut files: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("requests.") && name.ends_with(".jsonl"))
        .collect();
    // Newest files first, the same name order the Next route used.
    files.sort();
    files.reverse();

    let mut records: Vec<RequestLogRecord> = Vec::new();
    for file in files.iter().take(3) {
        let text = std::fs::read_to_string(Path::new(dir).join(file))?;
        for line in text.lines() {
            if let Ok(record) = serde_json::from_str::<RequestLogRecord>(line) {
                records.push(record);
            }
        }
    }
    records.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    records.truncate(limit as usize);
    Ok(records)
}

/// `GET /v1/admin/status` — liveness, log-dir disk use, recent WARN/ERROR
/// lines, and the process identity the UI shows in its footer.
///
/// Always 200: if this handler runs, the gateway is up. Everything else is
/// best-effort detail over the configured log directory.
pub async fn handle_admin_status(State(state): State<Arc<AppState>>) -> Response {
    let dir = state.config.logging.dir.as_deref();
    let disk = disk_usage(dir, state.config.logging.max_disk_mb);
    let uptime_seconds = STARTED
        .get()
        .map_or(0, |started| started.elapsed().as_secs());
    Json(serde_json::json!({
        "health": { "status": "ok" },
        "disk": {
            "used_bytes": disk.used_bytes,
            "max_bytes": disk.max_bytes,
            "percent": disk.percent,
        },
        "recentErrors": recent_error_lines(dir),
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "address": display_address(&state.config.server.host, state.config.server.port),
        "uptime_seconds": uptime_seconds,
    }))
    .into_response()
}

struct DiskUsage {
    used_bytes: u64,
    max_bytes: Option<u64>,
    percent: Option<f64>,
}

/// Regular files in the log directory, summed. A missing directory reads as
/// zero: the quota is what it is, an unreadable dir is a caller problem.
fn disk_usage(dir: Option<&str>, max_disk_mb: u64) -> DiskUsage {
    let mut used_bytes = 0u64;
    if let Some(dir) = dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        used_bytes += metadata.len();
                    }
                }
            }
        }
    }
    // Mirror the Next route: the quota exists only when a log dir is
    // configured, and a zero quota is "no quota" rather than 0%.
    let max_bytes = (dir.is_some() && max_disk_mb > 0).then(|| max_disk_mb * 1024 * 1024);
    let percent = max_bytes.map(|max| used_bytes as f64 / max as f64 * 100.0);
    DiskUsage {
        used_bytes,
        max_bytes,
        percent,
    }
}

/// Up to 10 `"{timestamp} [{level}] {message}"` lines from the two newest
/// application `.jsonl` files (never the `requests.` mirror or the `usage.`
/// mirror). Unreadable dir or files read as no errors.
fn recent_error_lines(dir: Option<&str>) -> Vec<String> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.ends_with(".jsonl")
                && !name.starts_with("requests.")
                && !name.starts_with("usage.")
        })
        .collect();
    files.sort();
    files.reverse();

    let mut errors = Vec::new();
    for file in files.iter().take(2) {
        let Ok(text) = std::fs::read_to_string(Path::new(dir).join(file)) else {
            continue;
        };
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let level = entry
                .get("level")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if level != "ERROR" && level != "WARN" {
                continue;
            }
            let timestamp = entry
                .get("timestamp")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let message = entry
                .get("message")
                .or_else(|| entry.get("msg"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            errors.push(format!("{timestamp} [{level}] {message}"));
            if errors.len() >= 10 {
                break;
            }
        }
        if errors.len() >= 10 {
            break;
        }
    }
    errors
}

/// The bound address with a wildcard host shown as loopback, so the UI's
/// "copy address" button never hands out `0.0.0.0`.
fn display_address(host: &str, port: u16) -> String {
    let host = match host {
        "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        other => other,
    };
    format!("{host}:{port}")
}

/// `POST /v1/admin/convert` — the conversion playground.
///
/// Success and failure share the `ConvertApiResponse` shape
/// `{"output": "...", "error": "..."}`, exactly like the Next route and the
/// bridge call it replaced.
#[derive(Debug, Deserialize)]
pub struct ConvertBody {
    input: String,
    from: String,
    to: String,
    mode: String,
}

pub async fn handle_admin_convert(body: Result<Json<ConvertBody>, JsonRejection>) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return convert_error_response(StatusCode::BAD_REQUEST, "invalid request body"),
    };
    if body.input.is_empty() || body.from.is_empty() || body.to.is_empty() || body.mode.is_empty() {
        return convert_error_response(StatusCode::BAD_REQUEST, "invalid request body");
    }
    let from = match Format::parse(&body.from) {
        Ok(format) => format,
        Err(err) => return convert_error_response(StatusCode::BAD_REQUEST, err),
    };
    let to = match Format::parse(&body.to) {
        Ok(format) => format,
        Err(err) => return convert_error_response(StatusCode::BAD_REQUEST, err),
    };
    let converted = match body.mode.as_str() {
        "request" => safe_convert_request(body.input.as_bytes(), from, to),
        "response" => safe_convert_response(body.input.as_bytes(), from, to),
        other => {
            return convert_error_response(
                StatusCode::BAD_REQUEST,
                format!("unknown mode: {other}"),
            );
        }
    };
    let bytes = match converted {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("conversion failed from={} to={}: {err}", body.from, body.to);
            return convert_error_response(StatusCode::INTERNAL_SERVER_ERROR, err);
        }
    };
    match String::from_utf8(bytes) {
        Ok(output) => Json(serde_json::json!({ "output": output })).into_response(),
        Err(_) => convert_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "conversion produced non-UTF-8 output",
        ),
    }
}

fn convert_error_response(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (
        status,
        Json(serde_json::json!({ "output": "", "error": message.to_string() })),
    )
        .into_response()
}
