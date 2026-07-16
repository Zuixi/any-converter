use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use any_converter_core::convert::Format;
use any_converter_core::ir::Usage;
use chrono::Utc;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::config::RequestLogConfig;
use crate::request_log::redactor::{SanitizedBody, sanitize_body};

pub mod redactor;

/// Context accumulated throughout a request lifecycle for logging.
#[derive(Clone)]
pub struct RequestLogContext {
    pub req_id: uuid::Uuid,
    pub start_time: Instant,
    pub client_format: Format,
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
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ResponseBodyKind {
    Json { text: String },
    SseLines { lines: Vec<String> },
}

/// A single request/response log record written as one JSON Lines entry.
#[derive(Debug, Clone, Serialize)]
pub struct RequestLogRecord {
    pub request_id: String,
    pub timestamp: String,
    pub client_format: String,
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
        let (tx, mut rx) = mpsc::channel::<RequestLogRecord>(256);
        tokio::spawn(async move {
            while let Some(record) = rx.recv().await {
                if let Err(e) = write_record(&log_dir, &record) {
                    log::error!("failed to write request log record: {e}");
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

    let record = RequestLogRecord {
        request_id: ctx.req_id.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        client_format: ctx.client_format.to_string(),
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
    time_to_first_byte_ms: u64,
    usage: Usage,
    response_truncated: bool,
    max_capture_bytes: usize,
) {
    let max = max_capture_bytes;
    let request_body = Some(sanitize_body(&ctx.request_body, max));
    let upstream_request_body = Some(sanitize_body(&ctx.upstream_request_body, max));

    let truncated = request_body.as_ref().is_some_and(|b| b.truncated)
        || upstream_request_body.as_ref().is_some_and(|b| b.truncated)
        || response_truncated;

    let record = RequestLogRecord {
        request_id: ctx.req_id.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        client_format: ctx.client_format.to_string(),
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
    fn test_log_record_serialization() {
        let record = RequestLogRecord {
            request_id: "req-1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            client_format: "claude".to_string(),
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
            truncated: false,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"request_id\":\"req-1\""));
        assert!(json.contains("\"latency_ms\":123"));
    }
}
