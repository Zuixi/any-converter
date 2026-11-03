#![allow(clippy::unwrap_used)]

use any_converter_core::ir::Usage;
use any_converter_server::request_log::{RequestLogRecord, ResponseBodyKind};
use any_converter_server::storage::{RetentionPolicy, RetentionStats, SqliteStorage};

/// One `rollup_hourly` row, ordered by its key, for comparing against a direct aggregate.
#[derive(Debug, Clone, PartialEq)]
struct RollupRow {
    bucket: String,
    provider: String,
    client_model: String,
    agent: String,
    calls: u64,
    errors: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cache_write_1h: u64,
    cache_unknown_tokens: u64,
    reasoning_tokens: u64,
    latency_sum_ms: u64,
    latency_max_ms: u64,
    ttft_sum_ms: u64,
    ttft_calls: u64,
    cost_usd: f64,
}

fn rollup_rows(db_path: &std::path::Path) -> Vec<RollupRow> {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    collect_rollup_rows(
        &conn,
        r#"
        select bucket, provider, client_model, agent,
               calls, errors, input_tokens, output_tokens,
               cache_read_tokens, cache_write_tokens, cache_write_1h,
               cache_unknown_tokens, reasoning_tokens,
               latency_sum_ms, latency_max_ms, ttft_sum_ms, ttft_calls, cost_usd
        from rollup_hourly
        order by bucket, provider, client_model, agent
        "#,
    )
}

fn collect_rollup_rows(conn: &rusqlite::Connection, sql: &str) -> Vec<RollupRow> {
    let mut stmt = conn.prepare(sql).unwrap();
    let rows = stmt
        .query_map([], |row| {
            Ok(RollupRow {
                bucket: row.get(0)?,
                provider: row.get(1)?,
                client_model: row.get(2)?,
                agent: row.get(3)?,
                calls: row.get(4)?,
                errors: row.get(5)?,
                input_tokens: row.get(6)?,
                output_tokens: row.get(7)?,
                cache_read_tokens: row.get(8)?,
                cache_write_tokens: row.get(9)?,
                cache_write_1h: row.get(10)?,
                cache_unknown_tokens: row.get(11)?,
                reasoning_tokens: row.get(12)?,
                latency_sum_ms: row.get(13)?,
                latency_max_ms: row.get(14)?,
                ttft_sum_ms: row.get(15)?,
                ttft_calls: row.get(16)?,
                cost_usd: row.get(17)?,
            })
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
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

/// A fixed instant, so the day arithmetic below is exact.
fn utc(text: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// The default record, `days_ago` before `now`, in the format the log table stores.
fn record_days_ago(
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

/// Whether `table` still holds `request_id`. Every `table` here is a literal.
fn keeps(conn: &rusqlite::Connection, table: &str, request_id: &str) -> bool {
    conn.query_row(
        &format!("select exists(select 1 from {table} where request_id = ?1)"),
        [request_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn scalar(conn: &rusqlite::Connection, sql: &str) -> u64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn apply_retention_expires_bodies_days_before_the_logs_they_came_from() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let now = utc("2026-07-16T12:00:00Z");

    for (request_id, days_ago) in [("req-40d", 40), ("req-10d", 10), ("req-1d", 1)] {
        storage
            .insert_request_log(&record_days_ago(request_id, now, days_ago))
            .unwrap();
    }
    assert_eq!(storage.body_row_count().unwrap(), 3);

    let stats = storage
        .apply_retention(&RetentionPolicy::default(), now)
        .unwrap();
    // 10 and 40 days are both past the 7-day body window; only 40 is past the 30-day
    // log window, and the rollups are kept forever by default.
    assert_eq!(stats.bodies_deleted, 2);
    assert_eq!(stats.logs_deleted, 1);
    assert_eq!(stats.rollup_deleted, 0);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert!(!keeps(&conn, "request_logs", "req-40d"));
    assert!(!keeps(&conn, "request_bodies", "req-40d"));
    assert!(
        keeps(&conn, "request_logs", "req-10d"),
        "the log outlives its body"
    );
    assert!(!keeps(&conn, "request_bodies", "req-10d"));
    assert!(keeps(&conn, "request_logs", "req-1d"));
    assert!(keeps(&conn, "request_bodies", "req-1d"));

    // A second sweep has nothing left to do: the deletes are by age, not by cursor.
    let again = storage
        .apply_retention(&RetentionPolicy::default(), now)
        .unwrap();
    assert_eq!(again, RetentionStats::default());
}

#[test]
fn body_days_zero_keeps_bodies_for_every_log_that_survives() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let now = utc("2026-07-16T12:00:00Z");

    for (request_id, days_ago) in [("req-40d", 40), ("req-10d", 10)] {
        storage
            .insert_request_log(&record_days_ago(request_id, now, days_ago))
            .unwrap();
    }

    let policy = RetentionPolicy {
        rollup_days: 0,
        log_days: 30,
        body_days: 0,
    };
    let stats = storage.apply_retention(&policy, now).unwrap();
    assert_eq!(stats.bodies_deleted, 0, "bodies are kept forever");
    assert_eq!(stats.logs_deleted, 1, "logs still expire");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    // The 10-day body is the one the default 7-day window would have taken.
    assert_eq!(scalar(&conn, "select count(*) from request_bodies"), 1);
    assert!(keeps(&conn, "request_bodies", "req-10d"));
    assert!(!keeps(&conn, "request_bodies", "req-40d"), "cascade");
}

#[test]
fn rollup_days_deletes_whole_buckets_and_keeps_the_one_holding_the_cutoff_instant() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    // Off the hour on purpose: the 30-day cutoff lands at 12:30, and flooring it to
    // 12:00 must keep the bucket that still contains that instant.
    let now = utc("2026-07-16T12:30:00Z");

    let mut records = Vec::new();
    for (request_id, timestamp) in [
        ("req-past-hour", "2026-06-16T11:45:00Z"),
        ("req-in-cutoff-hour", "2026-06-16T12:45:00Z"),
        ("req-recent", "2026-07-16T12:00:00Z"),
    ] {
        let mut record = test_request_record(request_id);
        record.timestamp = timestamp.to_string();
        records.push(record);
    }
    storage.insert_request_logs(&records).unwrap();
    assert_eq!(rollup_rows(&db_path).len(), 3);

    let policy = RetentionPolicy {
        rollup_days: 30,
        log_days: 0,
        body_days: 0,
    };
    let stats = storage.apply_retention(&policy, now).unwrap();
    assert_eq!(stats.rollup_deleted, 1, "one bucket is older than 30 days");
    assert_eq!(stats.logs_deleted, 0);
    assert_eq!(stats.bodies_deleted, 0);

    let buckets: Vec<String> = rollup_rows(&db_path)
        .into_iter()
        .map(|row| row.bucket)
        .collect();
    assert_eq!(
        buckets,
        ["2026-06-16T12:00:00Z", "2026-07-16T12:00:00Z"],
        "the truncated buckets are gone, the boundary one stays"
    );
}

#[test]
fn apply_retention_frees_pages_for_incremental_vacuum() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let now = utc("2026-07-16T12:00:00Z");

    let mut template = test_request_record("req-bulk");
    template.timestamp = (now - chrono::Duration::days(40))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
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

    let stats = storage
        .apply_retention(&RetentionPolicy::default(), now)
        .unwrap();
    assert_eq!(stats.bodies_deleted, 200);
    assert_eq!(stats.logs_deleted, 200);
    assert_eq!(storage.body_row_count().unwrap(), 0);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let freelist_before = scalar(&conn, "pragma freelist_count");
    assert!(freelist_before > 0, "the sweep left no free pages");

    let released = storage.incremental_vacuum(100_000).unwrap();
    assert_eq!(released, freelist_before);

    let freelist_after = scalar(&conn, "pragma freelist_count");
    assert!(
        freelist_after < freelist_before,
        "freelist did not drop: {freelist_after} of {freelist_before}"
    );
    assert_eq!(freelist_after, 0);
}

#[test]
fn inserted_body_carries_the_record_timestamp() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();

    let mut record = test_request_record("req-body-ts");
    record.timestamp = "2026-07-16T13:41:07Z".to_string();
    storage.insert_request_logs(&[record.clone()]).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let body_timestamp: Option<String> = conn
        .query_row(
            "select timestamp from request_bodies where request_id = ?1",
            [&record.request_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(body_timestamp.as_deref(), Some("2026-07-16T13:41:07Z"));

    // A replay rewrites the body, so its age has to follow the corrected timestamp.
    let mut replay = record.clone();
    replay.timestamp = "2026-07-17T08:00:00Z".to_string();
    storage.insert_request_log(&replay).unwrap();
    let replayed: Option<String> = conn
        .query_row(
            "select timestamp from request_bodies where request_id = ?1",
            [&record.request_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replayed.as_deref(), Some("2026-07-17T08:00:00Z"));
    assert_eq!(storage.body_row_count().unwrap(), 1);
}

#[test]
fn schema_v6_backfills_the_timestamps_of_bodies_written_before_it() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    {
        let storage = SqliteStorage::open(&db_path).unwrap();
        let mut record = test_request_record("req-legacy-body");
        record.timestamp = "2026-07-16T09:15:00Z".to_string();
        storage.insert_request_log(&record).unwrap();
    }

    // Wind the body table back to v5: no timestamp column, no index. The row itself
    // stays, which is exactly the state the migration has to repair.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            drop index if exists idx_request_bodies_timestamp;
            alter table request_bodies drop column timestamp;
            pragma user_version = 5;
            "#,
        )
        .unwrap();
    }

    let storage = SqliteStorage::open(&db_path).unwrap();
    assert_eq!(storage.schema_version().unwrap(), 11);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let backfilled: Option<String> = conn
        .query_row(
            "select timestamp from request_bodies where request_id = ?1",
            ["req-legacy-body"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        backfilled.as_deref(),
        Some("2026-07-16T09:15:00Z"),
        "the old body took its log's timestamp"
    );
}
