#![allow(clippy::unwrap_used)]

use any_converter_core::ir::Usage;
use any_converter_server::request_log::{
    RequestLogRecord, RequestLogger, ResponseBodyKind, create_request_logger,
};
use any_converter_server::storage::SqliteStorage;
use any_converter_server::usage::UsageRecord;

#[test]
fn sqlite_storage_initializes_schema_and_persists_logs() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();

    let request = RequestLogRecord {
        request_id: "req-1".to_string(),
        timestamp: "2026-07-16T12:00:00Z".to_string(),
        client_format: "openai_responses".to_string(),
        client_id: None,
        session_id: None,
        provider: "kimi".to_string(),
        client_model: "gpt-5.4".to_string(),
        upstream_model: "moonshot-v1".to_string(),
        streaming: true,
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        request_body: None,
        upstream_request_body: None,
        response_status: 200,
        response_body: ResponseBodyKind::SseLines {
            lines: vec!["data: {\"type\":\"response.completed\"}".to_string()],
        },
        latency_ms: 1234,
        usage: Usage {
            input_tokens: 1122,
            output_tokens: 52,
            cache_read_tokens: Some(10),
            cache_write_tokens: None,
            reasoning_tokens: Some(4),
        },
        trace: None,
        truncated: false,
    };

    storage.insert_request_log(&request).unwrap();
    storage
        .insert_usage_record(&UsageRecord {
            request_id: "req-1".to_string(),
            timestamp: "2026-07-16T12:00:00Z".to_string(),
            client_format: "openai_responses".to_string(),
            provider: "kimi".to_string(),
            client_model: "gpt-5.4".to_string(),
            upstream_model: "moonshot-v1".to_string(),
            input_tokens: 1122,
            output_tokens: 52,
            total_tokens: 1174,
            latency_ms: 1234,
            status: 200,
            streaming: true,
        })
        .unwrap();

    let conn = rusqlite::Connection::open(db_path).unwrap();
    let saved_request: (String, String, u64, u64, u64, u64) = conn
        .query_row(
            "select request_id, response_body_kind, input_tokens, output_tokens, total_tokens, latency_ms from request_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .unwrap();
    assert_eq!(
        saved_request,
        ("req-1".to_string(), "sse".to_string(), 1122, 52, 1174, 1234)
    );

    let usage_total: u64 = conn
        .query_row(
            "select total_tokens from usage_logs where request_id = 'req-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(usage_total, 1174);
}

#[test]
fn sqlite_storage_reads_request_logs_and_hourly_usage() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    storage
        .insert_request_log(&test_request_record("req-1"))
        .unwrap();

    let mut second = test_request_record("req-2");
    second.timestamp = "2026-07-16T12:30:00Z".to_string();
    second.response_status = 500;
    second.usage.input_tokens = 100;
    second.usage.output_tokens = 25;
    second.latency_ms = 3000;
    storage.insert_request_log(&second).unwrap();

    let records = storage.recent_request_logs(10).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].request_id, "req-2");
    assert_eq!(records[1].request_id, "req-1");

    // Concurrent readonly open (Desktop Logs/Usage) while a writer connection is live.
    let readonly = SqliteStorage::open_readonly_in_log_dir(temp.path()).unwrap();
    let readonly_records = readonly.recent_request_logs(10).unwrap();
    assert_eq!(readonly_records.len(), 2);
    assert_eq!(readonly_records[0].request_id, "req-2");

    let usage = storage.hourly_usage_from_request_logs(50).unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].timestamp, "2026-07-16T12:00:00Z");
    assert_eq!(usage[0].input_tokens, 1222);
    assert_eq!(usage[0].output_tokens, 77);
    assert_eq!(usage[0].total_tokens, 1299);
    assert_eq!(usage[0].request_count, 2);
    assert_eq!(usage[0].error_count, 1);
    assert_eq!(usage[0].avg_latency_ms, 2117);
    assert_eq!(usage[0].max_latency_ms, 3000);
}

#[tokio::test]
async fn request_logger_writes_jsonl_and_sqlite() {
    let temp = tempfile::tempdir().unwrap();
    let log_dir = temp.path().to_path_buf();
    let logger = create_request_logger(
        Some(&log_dir.to_string_lossy()),
        &any_converter_server::config::RequestLogConfig {
            enabled: true,
            max_capture_bytes: 1024,
            trace_enabled: true,
            trace_max_preview_bytes: 512,
        },
    )
    .unwrap();

    logger.log(test_request_record("req-logger"));
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let jsonl_files: Vec<_> = std::fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("requests."))
        })
        .collect();
    assert_eq!(jsonl_files.len(), 1);
    let jsonl = std::fs::read_to_string(&jsonl_files[0]).unwrap();
    assert!(jsonl.contains("\"request_id\":\"req-logger\""));

    let conn = rusqlite::Connection::open(log_dir.join("any-converter.sqlite3")).unwrap();
    let count: u64 = conn
        .query_row(
            "select count(*) from request_logs where request_id = 'req-logger'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn request_logger_rotates_files_before_ten_mebibytes() {
    let temp = tempfile::tempdir().unwrap();
    let log_dir = temp.path().to_path_buf();
    let logger = RequestLogger::with_sqlite(log_dir.clone(), None);
    let mut first = test_request_record("req-large-1");
    first.response_body = ResponseBodyKind::Json {
        text: "x".repeat(6 * 1024 * 1024),
    };
    let mut second = first.clone();
    second.request_id = "req-large-2".to_string();

    logger.log(first);
    logger.log(second);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let jsonl_count = std::fs::read_dir(log_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("requests.") && name.ends_with(".jsonl"))
        })
        .count();
    assert_eq!(jsonl_count, 2);
}

fn test_request_record(request_id: &str) -> RequestLogRecord {
    RequestLogRecord {
        request_id: request_id.to_string(),
        timestamp: "2026-07-16T12:00:00Z".to_string(),
        client_format: "openai_responses".to_string(),
        client_id: None,
        session_id: None,
        provider: "kimi".to_string(),
        client_model: "gpt-5.4".to_string(),
        upstream_model: "moonshot-v1".to_string(),
        streaming: true,
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        request_body: None,
        upstream_request_body: None,
        response_status: 200,
        response_body: ResponseBodyKind::SseLines {
            lines: vec!["data: {\"type\":\"response.completed\"}".to_string()],
        },
        latency_ms: 1234,
        usage: Usage {
            input_tokens: 1122,
            output_tokens: 52,
            cache_read_tokens: Some(10),
            cache_write_tokens: None,
            reasoning_tokens: Some(4),
        },
        trace: None,
        truncated: false,
    }
}
