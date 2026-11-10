use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use any_converter_core::convert::Format;
use any_converter_core::ir::Usage;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::RequestLogConfig;
use crate::logging::{open_private_append, repair_private_dir};
use crate::observability::{RequestTraceSummary, summarize_request_trace, summarize_stream_trace};
use crate::request_log::redactor::{SanitizedBody, sanitize_body};
use crate::storage::{SqliteStorage, open_sqlite_storage_for_log_dir};

pub mod redactor;
use usage::extract_stream_usage_from_sse_lines;
pub mod usage;
pub use usage::{
    extract_effort, extract_kind, extract_served_model, extract_tier, extract_upstream_provider,
    extract_usage,
};

const MAX_REQUEST_LOG_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Flush the sqlite mirror once this many records are buffered.
const BATCH_MAX: usize = 256;
/// Flush the sqlite mirror after waiting this long for the next record.
const BATCH_FLUSH: Duration = Duration::from_millis(200);

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
    /// Account that answered the request (set after failover, when the provider has one).
    pub provider_account: Option<String>,
    /// The price the answering model was billed at, resolved when the provider
    /// was chosen. `None` = this model has no entry in `[pricing]`.
    pub price: Option<crate::pricing::Price>,
    /// Fingerprint of the price table that produced `price`.
    pub price_version: Option<String>,
    /// Zero-based index of the try that is answering this request right now.
    pub attempt: u32,
    /// Why that try was abandoned downstream. Reserved: nothing writes it yet,
    /// because only the answering try produces a row today.
    pub error: Option<String>,
    pub err_type: Option<String>,
    /// When the provider loop started its first try. `None` until then; this is
    /// the instant `own_ms` ends and the tries (and their pauses) begin.
    pub first_attempt_at: Option<Instant>,
    /// magpie's split of the request's time, in milliseconds. `own_ms` is our
    /// own work before the first byte left, `sent_ms` adds the failed tries and
    /// the pauses after them, `queued_ms` is time spent waiting for a slot
    /// (0: there is no such queue yet), `masking_ms` is of `own_ms` spent
    /// masking (0: there is no masking yet), and `flow_ms` is how long the
    /// reply's content took to arrive.
    pub own_ms: u64,
    pub sent_ms: u64,
    pub retried_ms: u64,
    pub queued_ms: u64,
    pub masking_ms: u64,
    pub flow_ms: u64,
    pub upstream_url: String,
    pub upstream_request_body: Vec<u8>,
    /// Where the call went and at which tier: `upstream` is the aggregator's
    /// provider (when one answers), `effort` the reasoning effort asked for,
    /// and the three tiers are asked by the client, sent upstream, and served by
    /// the vendor. `tier_served` is only ever what the reply named.
    pub upstream: Option<String>,
    pub effort: Option<String>,
    pub tier_asked: Option<String>,
    pub tier_sent: Option<String>,
    pub tier_served: Option<String>,
    /// Claude Code's delegation chain headers, when the client sends them.
    pub subagent: Option<String>,
    pub parent_agent: Option<String>,
    /// What kind of call this was, when the body says so (see
    /// [`usage::extract_kind`]).
    pub kind: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_model: Option<String>,
    #[serde(default)]
    pub ttft_ms: u64,
    #[serde(default)]
    pub cache_unknown: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_version: Option<String>,
    #[serde(default)]
    pub decode_ms: u64,
    #[serde(default)]
    pub decode_out: u64,
    /// magpie's time split, in milliseconds: our own work before the first byte
    /// left (`own_ms`), the same plus the failed tries and their pauses
    /// (`sent_ms`), the failed tries alone (`retried_ms`), time waiting for a
    /// concurrency/RPM slot (`queued_ms`), the masking part of `own_ms`
    /// (`masking_ms`), and how long the reply's content took to arrive
    /// (`flow_ms`).
    #[serde(default)]
    pub own_ms: u64,
    #[serde(default)]
    pub sent_ms: u64,
    #[serde(default)]
    pub retried_ms: u64,
    #[serde(default)]
    pub queued_ms: u64,
    #[serde(default)]
    pub masking_ms: u64,
    #[serde(default)]
    pub flow_ms: u64,
    /// Zero-based index of the try that answered: 0 when the first provider
    /// worked, 1 when it took a failover. magpie's `Tries` is this plus one.
    #[serde(default)]
    pub attempt: u32,
    /// The row of a try that failed and handed the request on. Reserved: this
    /// codebase writes only the answering try, so it is always false today.
    #[serde(default)]
    pub handed_on: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub err_type: Option<String>,
    /// Where the call went, and at which model tier: asked by the client, sent
    /// upstream, served by the vendor. `effort` is the reasoning effort, and
    /// `subagent`/`parent_agent` name the delegation chain, if any.
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub tier_asked: Option<String>,
    #[serde(default)]
    pub tier_sent: Option<String>,
    #[serde(default)]
    pub tier_served: Option<String>,
    #[serde(default)]
    pub subagent: Option<String>,
    #[serde(default)]
    pub parent_agent: Option<String>,
    /// What kind of call this was, when the client says so.
    #[serde(default)]
    pub kind: Option<String>,
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
            let mut batch: Vec<RequestLogRecord> = Vec::with_capacity(BATCH_MAX);
            loop {
                match tokio::time::timeout(BATCH_FLUSH, rx.recv()).await {
                    Ok(Some(record)) => {
                        // JSONL stays per record; only the sqlite mirror is batched.
                        if let Err(e) = write_record(&log_dir, &record) {
                            log::error!("failed to write request log record: {e}");
                        }
                        batch.push(record);
                        if batch.len() >= BATCH_MAX {
                            flush_sqlite(&sqlite, &mut batch);
                        }
                    }
                    // Channel closed: commit what is buffered before shutting down.
                    Ok(None) => {
                        flush_sqlite(&sqlite, &mut batch);
                        return;
                    }
                    // Quiet proxy: commit the partial batch so it is not stuck in memory.
                    Err(_timeout) => flush_sqlite(&sqlite, &mut batch),
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

/// Commit the buffered batch in one transaction, then drop it. A failing batch is
/// logged and discarded so the writer loop keeps running.
fn flush_sqlite(sqlite: &Option<SqliteStorage>, batch: &mut Vec<RequestLogRecord>) {
    if batch.is_empty() {
        return;
    }
    if let Some(storage) = sqlite {
        if let Err(e) = storage.insert_request_logs(batch) {
            log::error!("failed to write request log records to sqlite: {e}");
        }
    }
    batch.clear();
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
    repair_private_dir(log_dir)?;
    let date = &record.timestamp[..10];
    let line = serde_json::to_string(record).unwrap_or_default();
    let path = request_log_path(log_dir, date, line.len() as u64 + 1)?;
    let mut file = open_private_append(&path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

fn request_log_path(log_dir: &Path, date: &str, next_line_bytes: u64) -> std::io::Result<PathBuf> {
    // ponytail: linear segment scan is bounded by the directory quota; keep writer state only if profiling requires it.
    let mut index = 0;
    let mut latest = None;
    loop {
        let path = log_dir.join(format!("requests.{date}.{index:03}.jsonl"));
        match std::fs::metadata(&path) {
            Ok(metadata) => latest = Some((path, metadata.len())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return match latest {
                    Some((path, size)) if size + next_line_bytes <= MAX_REQUEST_LOG_FILE_BYTES => {
                        Ok(path)
                    }
                    _ => Ok(path),
                };
            }
            Err(error) => return Err(error),
        }
        index += 1;
    }
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

    let usage = extract_usage(upstream_body, provider_format);
    let record = RequestLogRecord {
        request_id: ctx.req_id.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        client_format: ctx.client_format.to_string(),
        client_id: ctx.client_id.clone(),
        session_id: ctx.session_id.clone(),
        provider: ctx.provider_name.clone(),
        client_model: ctx.client_model.clone(),
        upstream_model: ctx.upstream_model.clone(),
        provider_account: ctx.provider_account.clone(),
        served_model: extract_served_model(upstream_body, provider_format),
        ttft_ms: 0,
        cache_unknown: false,
        cost_usd: ctx.price.map(|p| p.cost_split(&usage)),
        price_version: if ctx.price.is_some() {
            ctx.price_version.clone()
        } else {
            None
        },
        decode_ms: 0,
        decode_out: 0,
        own_ms: ctx.own_ms,
        sent_ms: ctx.sent_ms,
        retried_ms: ctx.retried_ms,
        queued_ms: ctx.queued_ms,
        masking_ms: ctx.masking_ms,
        // A whole-body reply has no content stream, so it has no flow window.
        flow_ms: 0,
        attempt: ctx.attempt,
        handed_on: false,
        error: ctx.error.clone(),
        err_type: ctx.err_type.clone(),
        // Nothing on the non-streaming path knows these labels yet.
        upstream: ctx.upstream.clone(),
        effort: ctx.effort.clone(),
        tier_asked: ctx.tier_asked.clone(),
        tier_sent: ctx.tier_sent.clone(),
        tier_served: ctx.tier_served.clone(),
        subagent: ctx.subagent.clone(),
        parent_agent: ctx.parent_agent.clone(),
        kind: ctx.kind.clone(),
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
        usage,
        trace,
        truncated,
    };
    logger.log(record);
}

/// Build and log a streaming request/response record.
///
/// `decode_ms` is the content window the vendor took (first content block to
/// last) and `decode_out` the tokens written in it, so `decode_out / decode_ms`
/// is the speed the vendor actually wrote at. `flow_ms` is that same content
/// window: how long the reply took to arrive.
#[allow(clippy::too_many_arguments)] // logging boundary: needs context, captured lines, status, latency, usage, truncation, and size guard.
pub fn log_streaming(
    logger: &RequestLogger,
    ctx: &RequestLogContext,
    sse_lines: Vec<String>,
    status: reqwest::StatusCode,
    provider_format: Format,
    time_to_first_byte_ms: u64,
    decode_ms: u64,
    decode_out: u64,
    flow_ms: u64,
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

    // Must run before `sse_lines` is moved into the record below.
    let reply = sse_lines.join("\n");
    let served_model = extract_served_model(reply.as_bytes(), provider_format);
    // Same rule as the JSON path: the reply names the tier and the aggregator,
    // and neither is inferred when it names none.
    let tier_served = usage::extract_tier(reply.as_bytes());
    let upstream = usage::extract_upstream_provider(reply.as_bytes());

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
        provider_account: ctx.provider_account.clone(),
        served_model,
        ttft_ms: time_to_first_byte_ms,
        cache_unknown: false,
        cost_usd: ctx.price.map(|p| p.cost_split(&usage)),
        price_version: if ctx.price.is_some() {
            ctx.price_version.clone()
        } else {
            None
        },
        decode_ms,
        decode_out,
        own_ms: ctx.own_ms,
        sent_ms: ctx.sent_ms,
        retried_ms: ctx.retried_ms,
        queued_ms: ctx.queued_ms,
        masking_ms: ctx.masking_ms,
        flow_ms,
        attempt: ctx.attempt,
        handed_on: false,
        error: ctx.error.clone(),
        err_type: ctx.err_type.clone(),
        // The reply side was read from the SSE events above; the ask side rides
        // on the context. No router rewrites the tier yet, so what was sent is
        // what the client asked for.
        upstream,
        effort: ctx.effort.clone(),
        tier_asked: ctx.tier_asked.clone(),
        tier_sent: ctx.tier_sent.clone().or_else(|| ctx.tier_asked.clone()),
        tier_served,
        subagent: ctx.subagent.clone(),
        parent_agent: ctx.parent_agent.clone(),
        kind: ctx.kind.clone(),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_log_record_serialization() {
        let record = RequestLogRecord {
            request_id: "req-1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            client_format: "claude".to_string(),
            client_id: None,
            session_id: None,
            provider_account: None,
            served_model: None,
            ttft_ms: 0,
            cache_unknown: false,
            cost_usd: None,
            price_version: None,
            decode_ms: 0,
            decode_out: 0,
            own_ms: 0,
            sent_ms: 0,
            retried_ms: 0,
            queued_ms: 0,
            masking_ms: 0,
            flow_ms: 0,
            attempt: 0,
            handed_on: false,
            error: None,
            err_type: None,
            upstream: None,
            effort: None,
            tier_asked: None,
            tier_sent: None,
            tier_served: None,
            subagent: None,
            parent_agent: None,
            kind: None,
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
                cache_write_1h: None,
                reasoning_tokens: None,
            },
            trace: None,
            truncated: false,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"request_id\":\"req-1\""));
        assert!(json.contains("\"latency_ms\":123"));
    }

    fn test_ctx(provider_account: Option<String>) -> RequestLogContext {
        RequestLogContext {
            req_id: uuid::Uuid::nil(),
            start_time: Instant::now(),
            client_format: Format::Claude,
            client_id: None,
            session_id: None,
            client_model: "claude-3".to_string(),
            upstream_model: "gpt-4".to_string(),
            streaming: false,
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            request_body: br#"{"model":"claude-3"}"#.to_vec(),
            provider_name: "openai".to_string(),
            provider_account,
            price: None,
            price_version: None,
            attempt: 0,
            error: None,
            err_type: None,
            first_attempt_at: None,
            own_ms: 0,
            sent_ms: 0,
            retried_ms: 0,
            queued_ms: 0,
            masking_ms: 0,
            flow_ms: 0,
            upstream_url: "http://upstream/v1/chat/completions".to_string(),
            upstream_request_body: br#"{"model":"gpt-4"}"#.to_vec(),
            upstream: None,
            effort: None,
            tier_asked: None,
            tier_sent: None,
            tier_served: None,
            subagent: None,
            parent_agent: None,
            kind: None,
        }
    }

    fn price(input: f64, output: f64) -> crate::pricing::Price {
        // `Price` keeps no `Default`: the zero-valued cache rates have to be
        // spelled out or they would silently bill cache traffic.
        crate::pricing::Price {
            input,
            output,
            cache_read: 0.0,
            cache_write: 0.0,
            cache_write_1h: 0.0,
        }
    }

    /// One million input tokens, no output: a cost that reads off the rate.
    const ONE_MILLION_INPUT: &[u8] =
        br#"{"usage":{"prompt_tokens":1000000,"completion_tokens":0}}"#;

    async fn record_for_price(price: Option<crate::pricing::Price>) -> RequestLogRecord {
        let dir = tempfile::tempdir().unwrap();
        let logger = RequestLogger::new(dir.path().to_path_buf());
        let mut ctx = test_ctx(None);
        ctx.price = price;
        ctx.price_version = price.map(|_| "v1".to_string());

        log_non_streaming(
            &logger,
            &ctx,
            ONE_MILLION_INPUT,
            br#"{"id":"ok"}"#,
            reqwest::StatusCode::OK,
            Format::OpenAIChat,
            &RequestLogConfig::default(),
        );

        wait_for_record(dir.path()).await
    }

    fn read_first_record(dir: &Path) -> Option<RequestLogRecord> {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let text = std::fs::read_to_string(&path).ok()?;
            if let Some(line) = text.lines().find(|line| !line.is_empty()) {
                return serde_json::from_str::<RequestLogRecord>(line).ok();
            }
        }
        None
    }

    async fn wait_for_record(dir: &Path) -> RequestLogRecord {
        for _ in 0..100 {
            if let Some(record) = read_first_record(dir) {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        read_first_record(dir).unwrap()
    }

    #[tokio::test]
    async fn non_streaming_record_keeps_served_model_and_provider_account() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RequestLogger::new(dir.path().to_path_buf());
        let ctx = test_ctx(Some("acct-1".to_string()));
        let upstream_body =
            br#"{"id":"ok","model":"gpt-4-2024-08-06","usage":{"prompt_tokens":1,"completion_tokens":1}}"#;

        log_non_streaming(
            &logger,
            &ctx,
            upstream_body,
            br#"{"id":"ok"}"#,
            reqwest::StatusCode::OK,
            Format::OpenAIChat,
            &RequestLogConfig::default(),
        );

        let record = wait_for_record(dir.path()).await;
        assert_eq!(record.served_model.as_deref(), Some("gpt-4-2024-08-06"));
        assert_eq!(record.provider_account.as_deref(), Some("acct-1"));
    }

    #[tokio::test]
    async fn record_round_trips_every_label() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RequestLogger::new(dir.path().to_path_buf());
        let mut ctx = test_ctx(None);
        ctx.upstream = Some("DeepInfra".to_string());
        ctx.effort = Some("high".to_string());
        ctx.tier_asked = Some("flex".to_string());
        ctx.tier_sent = Some("flex".to_string());
        ctx.tier_served = Some("default".to_string());
        ctx.subagent = Some("agent-1".to_string());
        ctx.parent_agent = Some("agent-0".to_string());
        ctx.kind = Some("auxiliary".to_string());

        log_non_streaming(
            &logger,
            &ctx,
            br#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
            br#"{"id":"ok"}"#,
            reqwest::StatusCode::OK,
            Format::OpenAIChat,
            &RequestLogConfig::default(),
        );

        let record = wait_for_record(dir.path()).await;
        assert_eq!(record.upstream.as_deref(), Some("DeepInfra"));
        assert_eq!(record.effort.as_deref(), Some("high"));
        assert_eq!(record.tier_asked.as_deref(), Some("flex"));
        assert_eq!(record.tier_sent.as_deref(), Some("flex"));
        assert_eq!(record.tier_served.as_deref(), Some("default"));
        assert_eq!(record.subagent.as_deref(), Some("agent-1"));
        assert_eq!(record.parent_agent.as_deref(), Some("agent-0"));
        assert_eq!(record.kind.as_deref(), Some("auxiliary"));
    }

    #[tokio::test]
    async fn streaming_record_keeps_served_model_and_leaves_account_none_for_key_auth() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RequestLogger::new(dir.path().to_path_buf());
        let ctx = test_ctx(None);
        let sse_lines = vec![
            "event: message_start".to_string(),
            concat!(
                r#"data: {"type":"message_start","message":{"id":"m1","model":""#,
                r#"claude-3-5-sonnet-20241022"}}"#
            )
            .to_string(),
            "".to_string(),
            "event: message_stop".to_string(),
            r#"data: {"type":"message_stop"}"#.to_string(),
        ];

        log_streaming(
            &logger,
            &ctx,
            sse_lines,
            reqwest::StatusCode::OK,
            Format::Claude,
            42,
            0,
            0,
            0,
            Usage::default(),
            false,
            1024 * 1024,
            true,
            2048,
        );

        let record = wait_for_record(dir.path()).await;
        assert_eq!(
            record.served_model.as_deref(),
            Some("claude-3-5-sonnet-20241022")
        );
        // A key-based provider has no account: the field stays absent, never invented.
        assert_eq!(record.provider_account, None);
    }

    #[tokio::test]
    async fn record_keeps_the_attempt_that_answered() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RequestLogger::new(dir.path().to_path_buf());
        let mut ctx = test_ctx(None);
        // Third provider answered: the first two were handed on.
        ctx.attempt = 2;

        log_non_streaming(
            &logger,
            &ctx,
            br#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
            br#"{"id":"ok"}"#,
            reqwest::StatusCode::OK,
            Format::OpenAIChat,
            &RequestLogConfig::default(),
        );

        let record = wait_for_record(dir.path()).await;
        assert_eq!(record.attempt, 2);
        assert!(!record.handed_on, "the answering row is never handed on");
    }

    #[test]
    fn old_record_without_failover_fields_still_deserializes() {
        let old = serde_json::json!({
            "request_id": "req-old",
            "timestamp": "2024-01-01T00:00:00Z",
            "client_format": "claude",
            "provider": "openai",
            "client_model": "claude-3",
            "upstream_model": "gpt-4",
            "streaming": false,
            "method": "POST",
            "path": "/v1/messages",
            "response_status": 200,
            "response_body": { "text": "{}" },
            "latency_ms": 123,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_read_tokens": null,
                "cache_write_tokens": null,
                "cache_write_1h": null,
                "reasoning_tokens": null
            },
            "truncated": false
        });

        let record: RequestLogRecord = serde_json::from_value(old).unwrap();
        assert_eq!(record.attempt, 0);
        assert!(!record.handed_on);
        assert_eq!(record.error, None);
        assert_eq!(record.err_type, None);
    }

    #[tokio::test]
    async fn priced_record_carries_the_cost() {
        let record = record_for_price(Some(price(3.0, 15.0))).await;
        assert_eq!(record.usage.input_tokens, 1_000_000);
        assert_eq!(record.cost_usd, Some(3.0));
    }

    #[tokio::test]
    async fn unpriced_record_has_no_cost_and_no_price_version() {
        let record = record_for_price(None).await;
        assert_eq!(record.cost_usd, None);
        assert_eq!(record.price_version, None);
    }

    #[tokio::test]
    async fn priced_record_names_the_price_table_version() {
        let record = record_for_price(Some(price(3.0, 15.0))).await;
        assert!(
            record.price_version.is_some_and(|v| !v.is_empty()),
            "a cost is useless without the table that produced it"
        );
    }

    /// Folds records into the totals the storage views report, so a test can ask
    /// `Totals` what the data says without a database in between.
    fn totals_of(records: &[RequestLogRecord]) -> crate::storage::Totals {
        records
            .iter()
            .fold(crate::storage::Totals::default(), |mut totals, record| {
                totals.decode_ms += record.decode_ms;
                totals.decode_out += record.decode_out;
                totals.decode_n += u64::from(record.decode_ms > 0 && record.decode_out > 0);
                totals
            })
    }

    #[tokio::test]
    async fn streaming_record_measures_the_speed_the_vendor_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RequestLogger::new(dir.path().to_path_buf());
        let ctx = test_ctx(None);
        // First content at 120 ms, last at 920 ms: an 800 ms window in which the
        // vendor wrote 400 tokens — 500 tokens per second.
        log_streaming(
            &logger,
            &ctx,
            vec!["data: {\"delta\":\"hello\"}".to_string()],
            reqwest::StatusCode::OK,
            Format::Claude,
            120,
            800,
            400,
            800,
            Usage::default(),
            false,
            1024 * 1024,
            true,
            2048,
        );

        let record = wait_for_record(dir.path()).await;
        assert_eq!(record.decode_ms, 800);
        assert_eq!(record.decode_out, 400);
        assert_eq!(record.flow_ms, 800);

        let totals = totals_of(std::slice::from_ref(&record));
        let speed = totals.speed().unwrap();
        assert!(
            (speed - 500.0).abs() < 0.001,
            "400 tokens in 800 ms is 500 tokens/s, got {speed}"
        );
    }

    #[tokio::test]
    async fn speed_is_none_for_a_reply_that_was_not_streamed() {
        let dir = tempfile::tempdir().unwrap();
        let logger = RequestLogger::new(dir.path().to_path_buf());
        let ctx = test_ctx(None);

        log_non_streaming(
            &logger,
            &ctx,
            br#"{"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
            br#"{"id":"ok"}"#,
            reqwest::StatusCode::OK,
            Format::OpenAIChat,
            &RequestLogConfig::default(),
        );

        let record = wait_for_record(dir.path()).await;
        assert_eq!(record.decode_ms, 0);
        assert_eq!(record.decode_out, 0);
        // A non-streaming reply has no content window, so it has no speed either:
        // an invented number would read as a slow vendor rather than no data.
        assert_eq!(totals_of(std::slice::from_ref(&record)).speed(), None);
    }
}
