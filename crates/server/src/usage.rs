use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::storage::{SqliteStorage, open_sqlite_storage_for_log_dir};

#[derive(Debug, Clone, Serialize)]
pub struct UsageRecord {
    pub request_id: String,
    pub timestamp: String,
    pub client_format: String,
    pub provider: String,
    pub client_model: String,
    pub upstream_model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub latency_ms: u64,
    pub status: u16,
    pub streaming: bool,
}

/// Async usage logger that writes JSON Lines to a file via a background task.
#[derive(Clone)]
pub struct UsageLogger {
    tx: mpsc::Sender<UsageRecord>,
}

impl UsageLogger {
    /// Spawn a background writer and return the logger handle.
    pub fn new(log_dir: PathBuf) -> Self {
        Self::with_sqlite(
            log_dir.clone(),
            open_sqlite_storage_for_log_dir(log_dir.to_str()),
        )
    }

    /// Spawn a background writer with an optional SQLite mirror.
    pub fn with_sqlite(log_dir: PathBuf, sqlite: Option<SqliteStorage>) -> Self {
        let (tx, mut rx) = mpsc::channel::<UsageRecord>(256);
        tokio::spawn(async move {
            while let Some(record) = rx.recv().await {
                if let Err(e) = write_record(&log_dir, &record) {
                    log::error!("failed to write usage record: {e}");
                }
                if let Some(ref storage) = sqlite {
                    if let Err(e) = storage.insert_usage_record(&record) {
                        log::error!("failed to write usage record to sqlite: {e}");
                    }
                }
            }
        });
        Self { tx }
    }

    pub fn log(&self, record: UsageRecord) {
        if let Err(e) = self.tx.try_send(record) {
            log::warn!("usage log channel full, dropping record: {e}");
        }
    }
}

fn write_record(log_dir: &PathBuf, record: &UsageRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(log_dir)?;
    let date = &record.timestamp[..10]; // YYYY-MM-DD
    let path = log_dir.join(format!("usage.{date}.jsonl"));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(record).unwrap_or_default();
    writeln!(file, "{line}")?;
    Ok(())
}

/// Extract token usage from a provider response body.
/// Supports OpenAI Chat, Claude, Gemini, and OpenAI Responses formats.
pub fn extract_usage_from_response(body: &[u8]) -> (u64, u64) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return (0, 0);
    };
    // OpenAI Chat / Responses: usage.prompt_tokens, usage.completion_tokens
    if let Some(usage) = v.get("usage") {
        let input = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return (input, output);
    }
    // Gemini: usageMetadata.promptTokenCount, usageMetadata.candidatesTokenCount
    if let Some(meta) = v.get("usageMetadata") {
        let input = meta
            .get("promptTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = meta
            .get("candidatesTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return (input, output);
    }
    (0, 0)
}

/// Create usage logger if a log directory is configured.
pub fn create_usage_logger(log_dir: Option<&str>) -> Option<Arc<UsageLogger>> {
    log_dir.map(|dir| Arc::new(UsageLogger::new(PathBuf::from(dir))))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_extract_usage_openai_chat() {
        let body = br#"{"id":"chatcmpl-1","usage":{"prompt_tokens":100,"completion_tokens":50}}"#;
        let (input, output) = extract_usage_from_response(body);
        assert_eq!(input, 100);
        assert_eq!(output, 50);
    }

    #[test]
    fn test_extract_usage_claude() {
        let body = br#"{"id":"msg_1","usage":{"input_tokens":200,"output_tokens":80}}"#;
        let (input, output) = extract_usage_from_response(body);
        assert_eq!(input, 200);
        assert_eq!(output, 80);
    }

    #[test]
    fn test_extract_usage_gemini() {
        let body = br#"{"candidates":[],"usageMetadata":{"promptTokenCount":150,"candidatesTokenCount":60}}"#;
        let (input, output) = extract_usage_from_response(body);
        assert_eq!(input, 150);
        assert_eq!(output, 60);
    }

    #[test]
    fn test_extract_usage_missing() {
        let body = br#"{"error":"bad request"}"#;
        let (input, output) = extract_usage_from_response(body);
        assert_eq!(input, 0);
        assert_eq!(output, 0);
    }
}
