#![allow(clippy::unwrap_used)]
//! End-to-end proof that the database stops growing once retention runs.

use any_converter_core::ir::Usage;
use any_converter_server::request_log::{RequestLogRecord, ResponseBodyKind};
use any_converter_server::storage::{RetentionPolicy, SqliteStorage};

fn record(i: usize, days_ago: i64, now: chrono::DateTime<chrono::Utc>) -> RequestLogRecord {
    // Anchor every row to ONE clock reading: deriving it per row lets the
    // 30-day boundary fall on either side of the cutoff between rows.
    let ts = now - chrono::Duration::days(days_ago);
    RequestLogRecord {
        request_id: format!("req-{i}"),
        timestamp: ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        client_format: "claude".into(),
        client_id: Some("agent-a".into()),
        session_id: Some(format!("s-{i}")),
        provider: "p".into(),
        client_model: "m".into(),
        upstream_model: "m".into(),
        streaming: true,
        method: "POST".into(),
        path: "/v1/messages".into(),
        request_body: None,
        upstream_request_body: None,
        response_status: 200,
        response_body: ResponseBodyKind::Json {
            text: "x".repeat(64 * 1024),
        },
        latency_ms: 100,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        trace: None,
        truncated: false,
        provider_account: None,
        served_model: None,
        ttft_ms: 50,
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
    }
}

#[test]
fn database_does_not_grow_without_bound() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db.sqlite3");
    let store = SqliteStorage::open(&path).unwrap();
    let policy = RetentionPolicy::default(); // rollup 0, log 30, body 7
    let now = chrono::Utc::now();

    // 40 days of history at 40 records/day, 64 KiB body each.
    for day in 0..40 {
        for i in 0..40 {
            store
                .insert_request_log(&record(day * 40 + i, day as i64, now))
                .unwrap();
        }
    }
    let before = std::fs::metadata(&path).unwrap().len();
    let bodies_before = store.body_row_count().unwrap();

    let stats = store.apply_retention(&policy, now).unwrap();
    let pages = store.incremental_vacuum(1_000_000).unwrap();
    store.checkpoint().unwrap();
    let after = std::fs::metadata(&path).unwrap().len();
    let bodies_after = store.body_row_count().unwrap();

    // 7-day body window over 40 days of history must drop most of the payload.
    assert!(bodies_after * 4 < bodies_before, "bodies did not expire");
    // 30-day metadata window: a row exactly 30 days old is NOT older than the
    // cutoff (strict `<`), so days 31..=39 go: 9 days x 40 records.
    assert_eq!(stats.logs_deleted, 360, "9 days x 40 records");
    // Bodies older than 7 days are gone.
    assert_eq!(stats.bodies_deleted, bodies_before - bodies_after);
    // And the file itself must shrink, not just the row count.
    assert!(
        after < before,
        "file did not shrink: {before} -> {after} (logs={}, bodies={}, pages={})",
        stats.logs_deleted,
        stats.bodies_deleted,
        pages
    );
}
