#![allow(clippy::unwrap_used)]

use any_converter_core::ir::Usage;
use any_converter_server::request_log::{
    RequestLogRecord, RequestLogger, ResponseBodyKind, create_request_logger,
};
use any_converter_server::storage::{Granularity, SqliteStorage, Totals, UsageQuery};

#[test]
fn sqlite_storage_initializes_schema_and_persists_logs() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    assert_eq!(storage.schema_version().unwrap(), 11);

    let request = RequestLogRecord {
        request_id: "req-1".to_string(),
        timestamp: "2026-07-16T12:00:00Z".to_string(),
        client_format: "openai_responses".to_string(),
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
        upstream: None,
        effort: None,
        tier_asked: None,
        tier_sent: None,
        tier_served: None,
        subagent: None,
        parent_agent: None,
        kind: None,
        attempt: 0,
        handed_on: false,
        error: None,
        err_type: None,
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
            cache_write_1h: None,
            reasoning_tokens: Some(4),
        },
        trace: None,
        truncated: false,
    };

    storage.insert_request_log(&request).unwrap();

    let conn = rusqlite::Connection::open(db_path).unwrap();
    let saved_request: (String, u64, u64, u64, u64) = conn
        .query_row(
            "select request_id, input_tokens, output_tokens, total_tokens, latency_ms from request_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(saved_request, ("req-1".to_string(), 1122, 52, 1174, 1234));

    // The wide payload lives only in `request_bodies` and round-trips byte-identically.
    let stored_json: String = conn
        .query_row(
            "select record_json from request_bodies where request_id = 'req-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_json, serde_json::to_string(&request).unwrap());
    let round_tripped: RequestLogRecord = serde_json::from_str(&stored_json).unwrap();
    assert_eq!(serde_json::to_string(&round_tripped).unwrap(), stored_json);

    // Schema v3 dropped the write-only `usage_logs` table entirely.
    let usage_logs_tables: u64 = conn
        .query_row(
            "select count(*) from sqlite_master where type='table' and name='usage_logs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(usage_logs_tables, 0);
}

#[test]
fn sqlite_storage_reads_request_logs_and_aggregates_them() {
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

    // Aggregation reads the rollup, and both calls fall in one hour.
    let query = UsageQuery {
        from: "2026-07-16T00:00:00Z".to_string(),
        to: "2026-07-17T00:00:00Z".to_string(),
        granularity: Granularity::Hour,
        provider: None,
        model: None,
        agent: None,
        account: None,
        split_by: None,
    };
    let summary = storage.usage_summary(&query).unwrap();
    assert_eq!(summary.totals.calls, 2);
    assert_eq!(summary.totals.errors, 1);
    assert_eq!(summary.totals.input_tokens, 1222);
    assert_eq!(summary.totals.output_tokens, 77);
    assert_eq!(summary.totals.latency_max_ms, 3000);
    assert_eq!(summary.series.len(), 1);
    assert_eq!(summary.series[0].timestamp, "2026-07-16T12:00:00Z");
}

#[test]
fn sqlite_storage_tracks_schema_version_and_reopens_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");

    let storage = SqliteStorage::open(&db_path).unwrap();
    assert_eq!(storage.schema_version().unwrap(), 11);
    storage
        .insert_request_log(&test_request_record("req-1"))
        .unwrap();
    drop(storage);

    // Re-opening an already migrated database must not re-run migrations or lose rows.
    let reopened = SqliteStorage::open(&db_path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 11);
    assert_eq!(reopened.recent_request_logs(10).unwrap().len(), 1);
}

#[test]
fn sqlite_storage_round_trips_statistics_columns() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    assert_eq!(storage.schema_version().unwrap(), 11);

    let mut record = test_request_record("req-stats");
    record.client_id = Some("claude-code".to_string());
    record.provider_account = Some("a@b.c".to_string());
    record.served_model = Some("moonshot-v2".to_string());
    record.ttft_ms = 321;
    record.cache_unknown = true;
    record.cost_usd = Some(0.5);
    record.price_version = Some("2026-08".to_string());
    record.decode_ms = 456;
    record.decode_out = 78;
    record.usage.cache_write_1h = Some(70);
    storage.insert_request_log(&record).unwrap();

    let records = storage.recent_request_logs(10).unwrap();
    assert_eq!(records.len(), 1);
    let saved = &records[0];
    assert_eq!(saved.client_id.as_deref(), Some("claude-code"));
    assert_eq!(saved.provider_account.as_deref(), Some("a@b.c"));
    assert_eq!(saved.served_model.as_deref(), Some("moonshot-v2"));
    assert_eq!(saved.ttft_ms, 321);
    assert!(saved.cache_unknown);
    assert_eq!(saved.cost_usd, Some(0.5));
    assert_eq!(saved.price_version.as_deref(), Some("2026-08"));
    assert_eq!(saved.decode_ms, 456);
    assert_eq!(saved.decode_out, 78);
    assert_eq!(saved.usage.cache_write_1h, Some(70));

    // The narrow row carries the columns, not just the wide JSON blob.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let row: (String, String, String, u64, bool, f64, u64, u64, u64) = conn
        .query_row(
            r#"
            select client_id, provider_account, served_model, ttft_ms,
                   cache_unknown, cost_usd, cache_write_1h, decode_ms, decode_out
            from request_logs where client_id = 'claude-code'
            "#,
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "claude-code".to_string(),
            "a@b.c".to_string(),
            "moonshot-v2".to_string(),
            321,
            true,
            0.5,
            70,
            456,
            78,
        )
    );
}

#[test]
fn sqlite_storage_round_trips_the_time_split() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();

    let mut record = test_request_record("req-split");
    // A call that took 40 ms to send, of which 35 ms were a failed try, and
    // whose content then arrived over 900 ms writing 400 tokens in 800 of them.
    record.own_ms = 5;
    record.sent_ms = 40;
    record.retried_ms = 35;
    record.flow_ms = 900;
    record.decode_ms = 800;
    record.decode_out = 400;
    record.upstream = Some("kimi".to_string());
    record.effort = Some("high".to_string());
    record.tier_asked = Some("smart".to_string());
    record.tier_sent = Some("smart".to_string());
    record.tier_served = Some("kimi-k2".to_string());
    record.subagent = Some("explore".to_string());
    record.parent_agent = Some("claude-code".to_string());
    record.kind = Some("chat".to_string());
    storage.insert_request_log(&record).unwrap();

    let saved = storage.recent_request_logs(10).unwrap().remove(0);
    assert_eq!(saved.own_ms, 5);
    assert_eq!(saved.sent_ms, 40);
    assert_eq!(saved.retried_ms, 35);
    assert_eq!(saved.queued_ms, 0, "there is no concurrency queue yet");
    assert_eq!(saved.masking_ms, 0, "there is no masking yet");
    assert_eq!(saved.flow_ms, 900);
    assert_eq!(saved.decode_ms, 800);
    assert_eq!(saved.decode_out, 400);
    assert_eq!(saved.upstream.as_deref(), Some("kimi"));
    assert_eq!(saved.effort.as_deref(), Some("high"));
    assert_eq!(saved.tier_asked.as_deref(), Some("smart"));
    assert_eq!(saved.tier_sent.as_deref(), Some("smart"));
    assert_eq!(saved.tier_served.as_deref(), Some("kimi-k2"));
    assert_eq!(saved.subagent.as_deref(), Some("explore"));
    assert_eq!(saved.parent_agent.as_deref(), Some("claude-code"));
    assert_eq!(saved.kind.as_deref(), Some("chat"));

    // The narrow row carries them too, not just the wide JSON blob: the usage
    // views sum the columns, and a column nobody writes sums to zero.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let row: (
        u64,
        u64,
        u64,
        u64,
        u64,
        String,
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            r#"
            select own_ms, sent_ms, retried_ms, flow_ms, decode_ms,
                   upstream, effort, tier_sent, subagent, kind
            from request_logs where request_id = 'req-split'
            "#,
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            5,
            40,
            35,
            900,
            800,
            "kimi".to_string(),
            "high".to_string(),
            "smart".to_string(),
            "explore".to_string(),
            "chat".to_string(),
        )
    );
}

#[test]
fn migration_v1_to_v2_moves_record_json_into_request_bodies() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let record = test_request_record("req-legacy");
    let record_json = serde_json::to_string(&record).unwrap();

    {
        // A version-1 database: wide body columns still on `request_logs`.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            create table request_logs (
                id integer primary key autoincrement,
                request_id text not null unique,
                timestamp text not null,
                client_format text not null,
                provider text not null,
                client_model text not null,
                upstream_model text not null,
                streaming integer not null,
                method text not null,
                path text not null,
                request_body_json text not null,
                upstream_request_body_json text not null,
                response_status integer not null,
                response_body_kind text not null,
                response_body_json text not null,
                latency_ms integer not null,
                input_tokens integer not null,
                output_tokens integer not null,
                total_tokens integer not null,
                cache_read_tokens integer,
                cache_write_tokens integer,
                reasoning_tokens integer,
                trace_json text not null,
                truncated integer not null,
                record_json text not null
            );
            pragma user_version = 1;
            "#,
        )
        .unwrap();
        conn.execute(
            r#"
            insert into request_logs (
                request_id, timestamp, client_format, provider, client_model,
                upstream_model, streaming, method, path, request_body_json,
                upstream_request_body_json, response_status, response_body_kind,
                response_body_json, latency_ms, input_tokens, output_tokens,
                total_tokens, trace_json, truncated, record_json
            ) values (
                'req-legacy', '2026-07-16T12:00:00Z', 'openai_responses', 'kimi', 'gpt-5.4',
                'moonshot-v1', 1, 'POST', '/v1/responses', 'null',
                'null', 200, 'sse',
                '{}', 1234, 1122, 52,
                1174, 'null', 0, ?1
            )
            "#,
            [&record_json],
        )
        .unwrap();
    }

    let storage = SqliteStorage::open(&db_path).unwrap();
    assert_eq!(storage.schema_version().unwrap(), 11);

    // The payload survived the copy, and the wide columns are gone from `request_logs`.
    let records = storage.recent_request_logs(10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(serde_json::to_string(&records[0]).unwrap(), record_json);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let narrow_columns: Vec<String> = conn
        .prepare("select name from pragma_table_info('request_logs')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for dropped in [
        "request_body_json",
        "upstream_request_body_json",
        "response_body_kind",
        "response_body_json",
        "trace_json",
        "record_json",
    ] {
        assert!(
            !narrow_columns.iter().any(|c| c == dropped),
            "{dropped} survived"
        );
    }
    // 25 columns at v1, 6 dropped by v2, 11 added by v4, 4 failover columns by v8,
    // 14 by v10 (the time split plus the labels).
    assert_eq!(narrow_columns.len(), 48);
}

#[test]
fn readonly_open_skips_migration() {
    let temp = tempfile::tempdir().unwrap();

    // Create an empty database file, then open it the way the Desktop Logs view does.
    drop(rusqlite::Connection::open(temp.path().join("any-converter.sqlite3")).unwrap());

    let readonly = SqliteStorage::open_readonly_in_log_dir(temp.path()).unwrap();
    assert_eq!(readonly.schema_version().unwrap(), 0);
    assert!(readonly.recent_request_logs(10).is_err());
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
    // Dropping the sender asks the writer to commit its partial batch. Poll
    // until the async task finishes instead of tying the test to wall-clock
    // timing while other integration suites compete for CPU.
    drop(logger);
    let database_path = log_dir.join("any-converter.sqlite3");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let count = loop {
        let count = rusqlite::Connection::open(&database_path)
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "select count(*) from request_logs where request_id = 'req-logger'",
                    [],
                    |row| row.get::<_, u64>(0),
                )
                .ok()
            })
            .unwrap_or(0);
        if count == 1 || std::time::Instant::now() >= deadline {
            break count;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };

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

#[test]
fn insert_request_logs_commits_300_records_in_one_call() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    let records: Vec<_> = (0..300)
        .map(|i| test_request_record(&format!("req-batch-{i}")))
        .collect();

    storage.insert_request_logs(&records).unwrap();

    assert_eq!(storage.recent_request_logs(400).unwrap().len(), 300);
}

#[test]
fn insert_request_logs_commits_neighbours_of_a_duplicate_record() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    storage
        .insert_request_log(&test_request_record("req-dup"))
        .unwrap();

    // The upsert makes the duplicate harmless, so the other two rows must still land.
    let records = vec![
        test_request_record("req-before"),
        test_request_record("req-dup"),
        test_request_record("req-after"),
    ];
    storage.insert_request_logs(&records).unwrap();

    assert_eq!(storage.recent_request_logs(10).unwrap().len(), 3);
}

#[tokio::test]
async fn request_logger_flushes_300_records_to_sqlite() {
    let temp = tempfile::tempdir().unwrap();
    let log_dir = temp.path().to_path_buf();
    let logger = RequestLogger::new(log_dir.clone());

    for i in 0..300 {
        logger.log(test_request_record(&format!("req-flush-{i}")));
        // The channel holds 256 records; yield so the writer drains it between sends.
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    drop(logger);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let storage = SqliteStorage::open_readonly_in_log_dir(&log_dir).unwrap();
    assert_eq!(storage.recent_request_logs(400).unwrap().len(), 300);
}

#[test]
fn prune_older_than_deletes_expired_logs_and_their_bodies() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-16T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    for (request_id, days_ago) in [("req-40d", 40), ("req-20d", 20), ("req-1d", 1)] {
        storage
            .insert_request_log(&record_at(request_id, now, days_ago))
            .unwrap();
    }

    assert_eq!(storage.prune_older_than(30, now).unwrap(), 1);

    let remaining: Vec<String> = storage
        .recent_request_logs(10)
        .unwrap()
        .into_iter()
        .map(|record| record.request_id)
        .collect();
    assert_eq!(remaining, vec!["req-1d", "req-20d"]);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let expired_bodies: u64 = conn
        .query_row(
            "select count(*) from request_bodies where request_id = 'req-40d'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(expired_bodies, 0);
    let kept_bodies: u64 = conn
        .query_row("select count(*) from request_bodies", [], |row| row.get(0))
        .unwrap();
    assert_eq!(kept_bodies, 2);
}

#[test]
fn prune_older_than_zero_retention_keeps_everything() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-16T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    storage
        .insert_request_log(&record_at("req-ancient", now, 4000))
        .unwrap();

    assert_eq!(storage.prune_older_than(0, now).unwrap(), 0);
    assert_eq!(storage.recent_request_logs(10).unwrap().len(), 1);
}

#[test]
fn prune_older_than_keeps_record_exactly_at_cutoff() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-16T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    storage
        .insert_request_log(&record_at("req-boundary", now, 30))
        .unwrap();

    assert_eq!(storage.prune_older_than(30, now).unwrap(), 0);
    assert_eq!(storage.recent_request_logs(10).unwrap().len(), 1);
}

fn record_at(
    request_id: &str,
    now: chrono::DateTime<chrono::Utc>,
    days_ago: i64,
) -> RequestLogRecord {
    let mut record = test_request_record(request_id);
    record.timestamp = (now - chrono::Duration::days(days_ago))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    record
}

#[test]
fn concurrent_writers_cascade_bodies_on_prune() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    // Four writers in flight at once cannot share one pooled connection, so the batch
    // writes below land on several of them. The cascade only exists if every one of
    // them has `foreign_keys = on`; a connection that does not leaves its body rows
    // orphaned and the prune reclaims kilobytes of narrow rows instead of the bodies.
    // (rusqlite's bundled sqlite also defaults foreign keys on, so this pins the
    // end-to-end outcome rather than the pragma itself.)
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for worker in 0..4 {
            let storage = &storage;
            let barrier = &barrier;
            scope.spawn(move || {
                let records: Vec<_> = (0..25)
                    .map(|i| test_request_record(&format!("req-{worker}-{i}")))
                    .collect();
                barrier.wait();
                storage.insert_request_logs(&records).unwrap();
            });
        }
    });

    assert_eq!(storage.body_row_count().unwrap(), 100);
    assert_eq!(storage.prune_before("2030-01-01T00:00:00Z").unwrap(), 100);
    assert_eq!(storage.body_row_count().unwrap(), 0);
}

#[test]
fn readonly_open_reads_rows_without_setting_journal_mode() {
    let temp = tempfile::tempdir().unwrap();
    {
        let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
        storage
            .insert_request_log(&test_request_record("req-ro"))
            .unwrap();
    }

    // A read-only connection must not try `journal_mode = wal`; its init pragmas are
    // only `busy_timeout` and `foreign_keys`, and reads keep working.
    let readonly = SqliteStorage::open_readonly_in_log_dir(temp.path()).unwrap();
    let records = readonly.recent_request_logs(10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].request_id, "req-ro");
}

#[test]
fn fresh_database_enables_incremental_auto_vacuum() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    assert_eq!(storage.schema_version().unwrap(), 11);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let auto_vacuum: i64 = conn
        .query_row("pragma auto_vacuum", [], |row| row.get(0))
        .unwrap();
    // 2 = incremental; without it `pragma incremental_vacuum` reclaims nothing.
    assert_eq!(auto_vacuum, 2, "auto_vacuum not incremental: {auto_vacuum}");
}

#[test]
fn incremental_vacuum_reclaims_pages_freed_by_prune() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();

    let mut template = test_request_record("req-bulk");
    template.response_body = ResponseBodyKind::Json {
        text: "x".repeat(32 * 1024),
    };
    let records: Vec<_> = (0..200)
        .map(|i| {
            let mut record = template.clone();
            record.request_id = format!("req-bulk-{i}");
            record
        })
        .collect();
    storage.insert_request_logs(&records).unwrap();
    assert_eq!(storage.body_row_count().unwrap(), 200);

    // The cascade has to take the bodies with the logs, or nothing is reclaimable.
    assert_eq!(storage.prune_before("2030-01-01T00:00:00Z").unwrap(), 200);
    assert_eq!(storage.body_row_count().unwrap(), 0);

    let freelist_before: u64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("pragma freelist_count", [], |row| row.get(0))
            .unwrap()
    };
    assert!(freelist_before > 0, "the delete left no free pages");

    let released = storage.incremental_vacuum(100_000).unwrap();
    assert_eq!(released, freelist_before);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let freelist_after: u64 = conn
        .query_row("pragma freelist_count", [], |row| row.get(0))
        .unwrap();
    assert_eq!(freelist_after, 0);
}

fn usage_query(granularity: Granularity) -> UsageQuery {
    UsageQuery {
        from: "2026-07-01T00:00:00Z".to_string(),
        to: "2026-08-01T00:00:00Z".to_string(),
        granularity,
        provider: None,
        model: None,
        agent: None,
        account: None,
        split_by: None,
    }
}

#[test]
fn hit_rate_is_none_without_history_and_zero_without_reads() {
    assert_eq!(Totals::default().hit_rate(), None);

    let no_reads = Totals {
        input_tokens: 100,
        ..Totals::default()
    };
    assert_eq!(no_reads.hit_rate(), Some(0.0));
}

#[test]
fn cache_unknown_prompt_tokens_leave_the_hit_rate_denominator() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut unknown = test_request_record("req-cache-unknown");
    unknown.cache_unknown = true;
    unknown.usage.input_tokens = 100;
    unknown.usage.cache_read_tokens = Some(50);
    let mut known = test_request_record("req-cache-known");
    known.usage.input_tokens = 100;
    known.usage.cache_read_tokens = Some(0);
    storage.insert_request_logs(&[unknown, known]).unwrap();

    let totals = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(totals.input_tokens, 200);
    assert_eq!(totals.cache_read_tokens, 50);
    assert_eq!(totals.cache_unknown_tokens, 150);
    // Denominator: 200 input + 50 reads - 150 uncounted prompt tokens = 100, so the
    // vendor that counted no cache never turns into a miss and the rate stays >= 0.
    assert_eq!(totals.hit_rate(), Some(0.5));

    // Only uncounted prompt tokens leave nothing to divide by: no data, not a miss.
    let only_unknown = Totals {
        input_tokens: 100,
        cache_read_tokens: 50,
        cache_unknown_tokens: 150,
        ..Totals::default()
    };
    assert_eq!(only_unknown.hit_rate(), None);
}

#[test]
fn usage_summary_filters_by_provider() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first = test_request_record("req-a");
    first.provider = "a".to_string();
    first.timestamp = "2026-07-16T12:00:00Z".to_string();
    first.usage.input_tokens = 100;
    first.usage.output_tokens = 10;
    let mut second = test_request_record("req-b");
    second.provider = "b".to_string();
    second.timestamp = "2026-07-16T13:00:00Z".to_string();
    second.usage.input_tokens = 700;
    second.usage.output_tokens = 70;
    storage.insert_request_logs(&[first, second]).unwrap();

    let mut query = usage_query(Granularity::Hour);
    query.provider = Some("a".to_string());
    let summary = storage.usage_summary(&query).unwrap();

    assert_eq!(summary.totals.calls, 1);
    assert_eq!(summary.totals.input_tokens, 100);
    assert_eq!(summary.totals.output_tokens, 10);
    assert_eq!(summary.series.len(), 1);
    assert_eq!(summary.series[0].timestamp, "2026-07-16T12:00:00Z");
    assert_eq!(summary.series[0].totals.input_tokens, 100);
    assert_eq!(summary.by_provider.len(), 1);
    assert_eq!(summary.by_provider[0].id, "a");
}

fn test_request_record(request_id: &str) -> RequestLogRecord {
    RequestLogRecord {
        request_id: request_id.to_string(),
        timestamp: "2026-07-16T12:00:00Z".to_string(),
        client_format: "openai_responses".to_string(),
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
        upstream: None,
        effort: None,
        tier_asked: None,
        tier_sent: None,
        tier_served: None,
        subagent: None,
        parent_agent: None,
        kind: None,
        attempt: 0,
        handed_on: false,
        error: None,
        err_type: None,
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
            cache_write_1h: None,
            reasoning_tokens: Some(4),
        },
        trace: None,
        truncated: false,
    }
}
