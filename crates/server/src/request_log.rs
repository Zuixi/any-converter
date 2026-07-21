use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use any_converter_core::convert::Format;
use any_converter_core::ir::Usage;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::RequestLogConfig;
use crate::observability::{RequestTraceSummary, summarize_request_trace, summarize_stream_trace};
use crate::request_log::redactor::{SanitizedBody, sanitize_body};
use crate::storage::{SqliteStorage, open_sqlite_storage_for_log_dir};

pub mod redactor;

/// Context accumulated throughout a request lifecycle for logging.
#[derive(Clone)]
pub struct RequestLogContext {
    pub req_id: uuid::Uuid,
    pub start_time: Instant,
    pub client_format: Format,
    /// Best-effort client identifier extracted from request headers.
    pub client_id: Option<String>,
    /// Session identifier extracted from request headers, if provided by the client.
    pub session_id: Option<String>,
    pub client_model: String,
    pub upstream_model: String,
    pub streaming: bool,
    pub method: String,
    pub path: String,
    pub request_body: Vec<u8>,
    pub provider_name: String,
    pub upstream_url: String,
    pub upstream_request_body: Vec<u8>,
}

/// A captured response body, either JSON for non-streaming or SSE lines for streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseBodyKind {
    Json { text: String },
    SseLines { lines: Vec<String> },
}

/// A single request/response log record written as one JSON Lines entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogRecord {
    pub request_id: String,
    pub timestamp: String,
    pub client_format: String,
    /// Best-effort client identifier extracted from request headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Session identifier extracted from request headers, if provided by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub provider: String,
    pub client_model: String,
    pub upstream_model: String,
    pub streaming: bool,
    pub method: String,
    pub path: String,
    pub request_body: Option<SanitizedBody>,
    pub upstream_request_body: Option<SanitizedBody>,
    pub response_status: u16,
    pub response_body: ResponseBodyKind,
    pub latency_ms: u64,
    pub usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<RequestTraceSummary>,
    pub truncated: bool,
}

/// Async request/response logger that writes JSON Lines to a file via a background task.
#[derive(Clone)]
pub struct RequestLogger {
    tx: mpsc::Sender<RequestLogRecord>,
}

impl RequestLogger {
    /// Spawn a background writer and return the logger handle.
    pub fn new(log_dir: PathBuf) -> Self {
        Self::with_sqlite(
            log_dir.clone(),
            open_sqlite_storage_for_log_dir(log_dir.to_str()),
        )
    }

    /// Spawn a background writer with an optional SQLite mirror.
    pub fn with_sqlite(log_dir: PathBuf, sqlite: Option<SqliteStorage>) -> Self {
        let (tx, mut rx) = mpsc::channel::<RequestLogRecord>(256);
        tokio::spawn(async move {
            while let Some(record) = rx.recv().await {
                if let Err(e) = write_record(&log_dir, &record) {
                    log::error!("failed to write request log record: {e}");
                }
                if let Some(ref storage) = sqlite {
                    if let Err(e) = storage.insert_request_log(&record) {
                        log::error!("failed to write request log record to sqlite: {e}");
                    }
                }
            }
        });
        Self { tx }
    }

    /// Enqueue a record for async writing.
    pub fn log(&self, record: RequestLogRecord) {
        if let Err(e) = self.tx.try_send(record) {
            log::warn!("request log channel full, dropping record: {e}");
        }
    }
}

/// Create a request logger if enabled and a log directory is configured.
pub fn create_request_logger(
    log_dir: Option<&str>,
    cfg: &RequestLogConfig,
) -> Option<Arc<RequestLogger>> {
    if !cfg.enabled {
        return None;
    }
    log_dir.map(|dir| Arc::new(RequestLogger::new(PathBuf::from(dir))))
}

fn write_record(log_dir: &Path, record: &RequestLogRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(log_dir)?;
    let date = &record.timestamp[..10];
    let path = log_dir.join(format!("requests.{date}.jsonl"));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(record).unwrap_or_default();
    writeln!(file, "{line}")?;
    Ok(())
}

/// Build and log a non-streaming request/response record.
pub fn log_non_streaming(
    logger: &RequestLogger,
    ctx: &RequestLogContext,
    upstream_body: &[u8],
    response_body: &[u8],
    status: reqwest::StatusCode,
    provider_format: Format,
    cfg: &RequestLogConfig,
) {
    let max = cfg.max_capture_bytes;
    let request_body = Some(sanitize_body(&ctx.request_body, max));
    let upstream_request_body = Some(sanitize_body(&ctx.upstream_request_body, max));
    let response_sanitized = sanitize_body(response_body, max);

    let truncated = request_body.as_ref().is_some_and(|b| b.truncated)
        || upstream_request_body.as_ref().is_some_and(|b| b.truncated)
        || response_sanitized.truncated;
    let trace = cfg.trace_enabled.then(|| {
        summarize_request_trace(
            &ctx.request_body,
            ctx.client_format,
            &ctx.upstream_request_body,
            provider_format,
            response_body,
            ctx.client_format,
            cfg.trace_max_preview_bytes,
        )
    });

    let record = RequestLogRecord {
        request_id: ctx.req_id.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        client_format: ctx.client_format.to_string(),
        client_id: ctx.client_id.clone(),
        session_id: ctx.session_id.clone(),
        provider: ctx.provider_name.clone(),
        client_model: ctx.client_model.clone(),
        upstream_model: ctx.upstream_model.clone(),
        streaming: false,
        method: ctx.method.clone(),
        path: ctx.path.clone(),
        request_body,
        upstream_request_body,
        response_status: status.as_u16(),
        response_body: ResponseBodyKind::Json {
            text: response_sanitized.text,
        },
        latency_ms: ctx.start_time.elapsed().as_millis() as u64,
        usage: extract_usage(upstream_body, provider_format),
        trace,
        truncated,
    };
    logger.log(record);
}

/// Build and log a streaming request/response record.
#[allow(clippy::too_many_arguments)] // logging boundary: needs context, captured lines, status, latency, usage, truncation, and size guard.
pub fn log_streaming(
    logger: &RequestLogger,
    ctx: &RequestLogContext,
    sse_lines: Vec<String>,
    status: reqwest::StatusCode,
    provider_format: Format,
    time_to_first_byte_ms: u64,
    usage: Usage,
    response_truncated: bool,
    max_capture_bytes: usize,
    trace_enabled: bool,
    trace_max_preview_bytes: usize,
) {
    let max = max_capture_bytes;
    let request_body = Some(sanitize_body(&ctx.request_body, max));
    let upstream_request_body = Some(sanitize_body(&ctx.upstream_request_body, max));

    let truncated = request_body.as_ref().is_some_and(|b| b.truncated)
        || upstream_request_body.as_ref().is_some_and(|b| b.truncated)
        || response_truncated;
    let trace = trace_enabled.then(|| {
        summarize_stream_trace(
            &ctx.request_body,
            ctx.client_format,
            &ctx.upstream_request_body,
            provider_format,
            &sse_lines,
            ctx.client_format,
            trace_max_preview_bytes,
        )
    });

    let stream_usage = extract_stream_usage_from_sse_lines(&sse_lines, ctx.client_format);
    let usage = if stream_usage.input_tokens > 0
        || stream_usage.output_tokens > 0
        || stream_usage.cache_read_tokens.is_some()
        || stream_usage.cache_write_tokens.is_some()
        || stream_usage.reasoning_tokens.is_some()
    {
        stream_usage
    } else {
        usage
    };

    let record = RequestLogRecord {
        request_id: ctx.req_id.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        client_format: ctx.client_format.to_string(),
        client_id: ctx.client_id.clone(),
        session_id: ctx.session_id.clone(),
        provider: ctx.provider_name.clone(),
        client_model: ctx.client_model.clone(),
        upstream_model: ctx.upstream_model.clone(),
        streaming: true,
        method: ctx.method.clone(),
        path: ctx.path.clone(),
        request_body,
        upstream_request_body,
        response_status: status.as_u16(),
        response_body: ResponseBodyKind::SseLines { lines: sse_lines },
        latency_ms: time_to_first_byte_ms,
        usage,
        trace,
        truncated,
    };
    logger.log(record);
}

/// Extract canonical token usage from a provider response body.
pub fn extract_usage(body: &[u8], format: Format) -> Usage {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Usage::default();
    };
    match format {
        Format::Claude => extract_claude_usage(&value),
        Format::Gemini => extract_gemini_usage(&value),
        Format::OpenAIChat | Format::OpenAIResponses => extract_openai_usage(&value),
    }
}

fn extract_openai_usage(value: &serde_json::Value) -> Usage {
    let Some(usage) = value.get("usage") else {
        return Usage::default();
    };
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64());
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64());
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens: None,
        reasoning_tokens,
    }
}

fn extract_claude_usage(value: &serde_json::Value) -> Usage {
    let Some(usage) = value.get("usage") else {
        return Usage::default();
    };
    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_write_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64());
    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64());
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens: None,
    }
}

fn extract_gemini_usage(value: &serde_json::Value) -> Usage {
    let Some(meta) = value.get("usageMetadata") else {
        return Usage::default();
    };
    let input_tokens = meta
        .get("promptTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = meta
        .get("candidatesTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    }
}

fn extract_stream_usage_from_sse_lines(lines: &[String], format: Format) -> Usage {
    let mut latest = Usage::default();
    for line in lines {
        for data in extract_sse_data_payloads(line) {
            if data == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            let usage = match format {
                Format::OpenAIResponses => value
                    .get("response")
                    .and_then(|response| response.get("usage"))
                    .map(|usage| extract_openai_usage(&serde_json::json!({ "usage": usage })))
                    .unwrap_or_default(),
                Format::OpenAIChat => extract_openai_usage(&value),
                Format::Claude => extract_claude_usage(&value).or_else(|| {
                    value
                        .get("message")
                        .and_then(|message| message.get("usage"))
                        .map(|usage| extract_claude_usage(&serde_json::json!({ "usage": usage })))
                        .unwrap_or_default()
                }),
                Format::Gemini => extract_gemini_usage(&value),
            };
            if usage.input_tokens > 0
                || usage.output_tokens > 0
                || usage.cache_read_tokens.is_some()
                || usage.cache_write_tokens.is_some()
                || usage.reasoning_tokens.is_some()
            {
                latest = usage;
            }
        }
    }
    latest
}

trait UsageExt {
    fn or_else<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self;
}

impl UsageExt for Usage {
    fn or_else<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self,
    {
        if self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.reasoning_tokens.is_some()
        {
            self
        } else {
            fallback()
        }
    }
}

fn extract_sse_data_payloads(block: &str) -> impl Iterator<Item = &str> {
    block
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_extract_usage_openai_chat() {
        let body = br#"{"id":"chatcmpl-1","usage":{"prompt_tokens":100,"completion_tokens":50}}"#;
        let usage = extract_usage(body, Format::OpenAIChat);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn test_extract_usage_claude() {
        let body = br#"{"id":"msg_1","usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":10}}"#;
        let usage = extract_usage(body, Format::Claude);
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cache_read_tokens, Some(10));
    }

    #[test]
    fn test_extract_usage_gemini() {
        let body = br#"{"candidates":[],"usageMetadata":{"promptTokenCount":150,"candidatesTokenCount":60}}"#;
        let usage = extract_usage(body, Format::Gemini);
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 60);
    }

    #[test]
    fn test_extract_usage_missing() {
        let body = br#"{"error":"bad request"}"#;
        let usage = extract_usage(body, Format::OpenAIChat);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn test_extract_usage_from_responses_completed_sse_lines() {
        let lines = vec![format!(
            "event: response.completed\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "usage": {
                        "input_tokens": 1122,
                        "output_tokens": 52,
                        "total_tokens": 1174
                    }
                }
            })
        )];

        let usage = extract_stream_usage_from_sse_lines(&lines, Format::OpenAIResponses);
        assert_eq!(usage.input_tokens, 1122);
        assert_eq!(usage.output_tokens, 52);
    }

    #[test]
    fn test_log_record_serialization() {
        let record = RequestLogRecord {
            request_id: "req-1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            client_format: "claude".to_string(),
            client_id: None,
            session_id: None,
            provider: "openai".to_string(),
            client_model: "claude-3".to_string(),
            upstream_model: "gpt-4".to_string(),
            streaming: false,
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            request_body: None,
            upstream_request_body: None,
            response_status: 200,
            response_body: ResponseBodyKind::Json {
                text: "{}".to_string(),
            },
            latency_ms: 123,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
            trace: None,
            truncated: false,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"request_id\":\"req-1\""));
        assert!(json.contains("\"latency_ms\":123"));
    }
}
