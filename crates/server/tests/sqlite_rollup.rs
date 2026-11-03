#![allow(clippy::unwrap_used)]

use any_converter_core::ir::Usage;
use any_converter_server::request_log::{RequestLogRecord, ResponseBodyKind};
use any_converter_server::storage::{Granularity, SqliteStorage, UsageQuery, UsageSummary};

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
fn rebuild_rollup_restores_a_summary_that_lost_a_rollup_row() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    // Three hours, two providers, two models, two agents: enough shapes that a
    // rebuild dropping any column expression would show up below.
    let mut first = test_request_record("req-a1");
    first.timestamp = "2026-07-16T12:05:00Z".to_string();
    first.client_id = Some("agent-a".to_string());
    first.usage.input_tokens = 100;

    let mut second = test_request_record("req-a2");
    second.timestamp = "2026-07-16T12:45:00Z".to_string();
    second.client_id = Some("agent-a".to_string());
    second.response_status = 500;
    second.usage.input_tokens = 200;

    let mut third = test_request_record("req-b1");
    third.timestamp = "2026-07-16T12:30:00Z".to_string();
    third.provider = "moonshot".to_string();
    third.client_model = "claude-x".to_string();
    third.usage.input_tokens = 300;

    let mut fourth = test_request_record("req-a3");
    fourth.timestamp = "2026-07-16T13:10:00Z".to_string();
    fourth.client_id = Some("agent-a".to_string());
    fourth.cache_unknown = true;
    fourth.ttft_ms = 300;
    fourth.cost_usd = Some(0.25);
    fourth.usage.input_tokens = 400;

    let mut fifth = test_request_record("req-c1");
    fifth.timestamp = "2026-07-17T09:00:00Z".to_string();
    fifth.provider = "moonshot".to_string();
    fifth.client_model = "claude-x".to_string();
    fifth.client_id = Some("agent-b".to_string());
    fifth.usage.input_tokens = 500;

    storage
        .insert_request_logs(&[first, second, third, fourth, fifth])
        .unwrap();

    let before = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();
    assert_eq!(before.totals.calls, 5);

    // Lose the whole 12:00 bucket by hand: the cache now lags the detail table.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let deleted = conn
        .execute(
            "delete from rollup_hourly where bucket = '2026-07-16T12:00:00Z'",
            [],
        )
        .unwrap();
    assert_eq!(deleted, 2, "two providers share that hour");

    let stale = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();
    assert_eq!(
        stale.totals.calls, 2,
        "the deleted bucket is gone from the cache"
    );
    assert!(stale.totals.input_tokens < before.totals.input_tokens);

    let written = storage.rebuild_rollup().unwrap();
    assert_eq!(
        written, 4,
        "two rows in 12:00, one in 13:00, one the next day"
    );
    assert_eq!(
        written as usize,
        rollup_rows(&db_path).len(),
        "the count reports the rows actually written"
    );
    assert_eq!(
        rollup_rows(&db_path),
        request_log_aggregates(&conn),
        "the rebuild reproduces the detail table exactly"
    );

    let after = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();
    assert_eq!(summary_json(&before), summary_json(&after));

    // Coarser granularities are folded from the same rows, so they agree as well.
    let daily = storage
        .usage_summary(&usage_query(Granularity::Day))
        .unwrap();
    assert_eq!(daily.totals.calls, 5);
    assert_eq!(daily.series.len(), 2, "two days of history");
    let daily_again = storage
        .usage_summary(&usage_query(Granularity::Day))
        .unwrap();
    assert_eq!(summary_json(&daily), summary_json(&daily_again));
}

/// One summary as JSON, so a change in any bucket or group shows up as one diff.
fn summary_json(summary: &UsageSummary) -> serde_json::Value {
    serde_json::to_value(summary).unwrap()
}

#[test]
fn rebuild_rollup_reproduces_the_unpriced_count_from_request_logs() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    // Two buckets and two agents, so a rebuild that lost the counter on one row
    // would still show up in the fold.
    let mut priced = test_request_record("req-rb-priced");
    priced.client_id = Some("agent-a".to_string());
    priced.cost_usd = Some(0.5);
    let mut unpriced_same_hour = test_request_record("req-rb-unpriced-a");
    unpriced_same_hour.client_id = Some("agent-a".to_string());
    unpriced_same_hour.timestamp = "2026-07-16T12:30:00Z".to_string();
    let mut unpriced_next_hour = test_request_record("req-rb-unpriced-b");
    unpriced_next_hour.client_id = Some("agent-b".to_string());
    unpriced_next_hour.timestamp = "2026-07-16T13:15:00Z".to_string();
    unpriced_next_hour.cost_usd = Some(0.1);
    let mut unpriced_third = test_request_record("req-rb-unpriced-c");
    unpriced_third.client_id = Some("agent-b".to_string());
    unpriced_third.timestamp = "2026-07-16T13:45:00Z".to_string();
    storage
        .insert_request_logs(&[
            priced,
            unpriced_same_hour,
            unpriced_next_hour,
            unpriced_third,
        ])
        .unwrap();

    let before = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();
    assert_eq!(before.totals.calls, 4);
    assert_eq!(before.totals.unpriced, 2);

    // Wipe and recompute from the detail table: the count comes back identical.
    storage.rebuild_rollup().unwrap();

    let after = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();
    assert_eq!(after.totals.unpriced, 2);
    assert_eq!(after.totals.cost_usd, before.totals.cost_usd);
    assert_eq!(summary_json(&before), summary_json(&after));

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        rollup_rows(&db_path),
        request_log_aggregates(&conn),
        "the rebuild reproduces `priced_calls` exactly"
    );
}

#[test]
fn usage_summary_does_not_count_a_failed_retry_and_rebuild_reproduces_both_counters() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    // A retry that never succeeded is an error, not a `Retried`.
    let mut failed_after_retry = test_request_record("req-failed-after-retry");
    failed_after_retry.attempt = 2;
    failed_after_retry.response_status = 500;
    // A loop ends without an answer, and its own counter says why.
    let mut looped = test_request_record("req-looped");
    looped.response_status = 500;
    looped.err_type = Some("loop".to_string());
    // The same bucket also holds a success that took a failover, so the rebuild
    // has to keep the two counters from bleeding into `errors`.
    let mut retried = test_request_record("req-retried");
    retried.attempt = 1;
    retried.response_status = 200;
    storage
        .insert_request_logs(&[failed_after_retry, looped, retried])
        .unwrap();

    let totals = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(totals.calls, 3);
    assert_eq!(totals.errors, 2);
    assert_eq!(totals.retried, 1, "the failed retry is not a success");
    assert_eq!(totals.loops, 1);

    storage.rebuild_rollup().unwrap();

    let rebuilt = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(rebuilt.errors, 2);
    assert_eq!(rebuilt.retried, 1, "the rebuild recomputes `Retried`");
    assert_eq!(rebuilt.loops, 1, "the rebuild recomputes `Loops`");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        rollup_rows(&db_path),
        request_log_aggregates(&conn),
        "a rebuilt row carries the same retried and loop counters"
    );
}

#[test]
fn replayed_request_id_does_not_double_count_priced_calls() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let mut first = test_request_record("req-replay-price");
    first.cost_usd = Some(0.25);
    storage.insert_request_log(&first).unwrap();

    // Same `request_id` replayed: still ONE priced call, like `calls`.
    let mut replay = test_request_record("req-replay-price");
    replay.cost_usd = Some(0.25);
    storage.insert_request_log(&replay).unwrap();

    let rows = rollup_rows(&db_path);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].calls, 1, "a replay is not a second call");
    assert_eq!(
        rows[0].priced_calls, 1,
        "a replay is not a second priced call"
    );

    let totals = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(totals.unpriced, 0);
    assert_eq!(totals.cost_usd, 0.25, "the price was counted once");
}

#[test]
fn rollup_hourly_merges_one_hour_of_one_agent_into_one_row() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let mut records = Vec::new();
    for (request_id, minute, latency_ms) in [
        ("req-a", "05", 300_u64),
        ("req-b", "25", 1500),
        ("req-c", "55", 700),
    ] {
        let mut record = test_request_record(request_id);
        record.timestamp = format!("2026-07-16T12:{minute}:00Z");
        record.client_id = Some("claude-code".to_string());
        record.latency_ms = latency_ms;
        record.ttft_ms = if latency_ms > 1000 { 400 } else { 0 };
        records.push(record);
    }
    storage.insert_request_logs(&records).unwrap();

    let rows = rollup_rows(&db_path);
    assert_eq!(rows.len(), 1, "one hour of one agent is one rollup row");
    let row = &rows[0];
    assert_eq!(row.bucket, "2026-07-16T12:00:00Z");
    assert_eq!(row.provider, "kimi");
    assert_eq!(row.client_model, "gpt-5.4");
    assert_eq!(row.agent, "claude-code");
    assert_eq!(row.calls, 3);
    assert_eq!(row.errors, 0);
    assert_eq!(row.input_tokens, 1122 * 3);
    assert_eq!(row.output_tokens, 52 * 3);
    assert_eq!(row.cache_read_tokens, 10 * 3);
    assert_eq!(row.cache_write_tokens, 0);
    assert_eq!(row.reasoning_tokens, 4 * 3);
    assert_eq!(row.latency_sum_ms, 300 + 1500 + 700);
    assert_eq!(
        row.latency_max_ms, 1500,
        "the high-water mark, not the last"
    );
    assert_eq!(row.ttft_sum_ms, 400);
    assert_eq!(row.ttft_calls, 1, "only calls that reported a ttft count");
}

#[test]
fn rollup_hourly_keeps_agents_of_the_same_hour_apart() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let mut first = test_request_record("req-agent-a");
    first.client_id = Some("agent-a".to_string());
    first.usage.input_tokens = 100;
    let mut second = test_request_record("req-agent-b");
    second.client_id = Some("agent-b".to_string());
    second.usage.input_tokens = 250;
    storage.insert_request_logs(&[first, second]).unwrap();

    let rows = rollup_rows(&db_path);
    assert_eq!(rows.len(), 2, "the agent is part of the rollup key");
    // `rollup_rows` orders by agent, so agent-a comes first.
    assert_eq!(rows[0].agent, "agent-a");
    assert_eq!(rows[0].calls, 1);
    assert_eq!(rows[0].input_tokens, 100);
    assert_eq!(rows[1].agent, "agent-b");
    assert_eq!(rows[1].calls, 1);
    assert_eq!(rows[1].input_tokens, 250);
}

#[test]
fn rollup_hourly_matches_the_equivalent_aggregate_over_request_logs() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    // Deterministic xorshift, so a failure is reproducible without a rng dependency.
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move || {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let mut pick = |modulus: u64| next() % modulus;

    let mut records = Vec::new();
    for index in 0..20 {
        let mut record = test_request_record(&format!("req-rand-{index}"));
        record.timestamp = format!("2026-07-16T{:02}:{:02}:00Z", pick(3) + 10, pick(60));
        record.provider = ["kimi", "moonshot"][pick(2) as usize].to_string();
        record.client_model = ["gpt-5.4", "claude-opus-5"][pick(2) as usize].to_string();
        record.client_id =
            [None, Some("agent-a"), Some("agent-b")][pick(3) as usize].map(str::to_string);
        record.response_status = [200, 200, 500][pick(3) as usize];
        record.usage.input_tokens = pick(10_000);
        record.usage.output_tokens = pick(1_000);
        record.usage.cache_read_tokens = pick(4).checked_sub(1).map(|value| value * 250);
        record.usage.cache_write_tokens = pick(4).checked_sub(1).map(|value| value * 125);
        record.usage.cache_write_1h = pick(3).checked_sub(1).map(|value| value * 60);
        record.usage.reasoning_tokens = pick(3).checked_sub(1).map(|value| value * 40);
        record.cache_unknown = pick(2) == 0;
        record.ttft_ms = if pick(4) == 0 { 0 } else { pick(900) };
        record.latency_ms = pick(5_000);
        record.cost_usd = Some((pick(4) * 25) as f64 / 100.0);
        records.push(record);
    }
    storage.insert_request_logs(&records).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let rollup = rollup_rows(&db_path);
    let aggregated = request_log_aggregates(&conn);

    assert_eq!(rollup.len(), aggregated.len());
    assert!(
        rollup.len() < records.len(),
        "20 records must fold into fewer rollup rows, got {}",
        rollup.len()
    );
    assert!(
        rollup.iter().any(|row| row.calls > 1),
        "no bucket was merged"
    );
    for (stored, expected) in rollup.iter().zip(aggregated.iter()) {
        assert_eq!(stored, expected, "rollup drifted from request_logs");
    }
    // Every record landed in exactly one bucket: nothing was lost by pre-aggregating.
    assert_eq!(
        rollup.iter().map(|row| row.calls).sum::<u64>(),
        records.len() as u64
    );
}

#[test]
fn rollup_hourly_adds_cache_unknown_prompt_tokens_only_when_flagged() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let mut unknown = test_request_record("req-unknown");
    unknown.client_id = Some("agent-a".to_string());
    unknown.cache_unknown = true;
    unknown.usage.input_tokens = 100;
    unknown.usage.cache_read_tokens = Some(50);
    unknown.usage.cache_write_tokens = Some(25);

    // Same hour, different agent: a separate row that must stay at zero.
    let mut known = test_request_record("req-known");
    known.client_id = Some("agent-b".to_string());
    known.usage.input_tokens = 1000;
    known.usage.cache_read_tokens = Some(400);
    known.usage.cache_write_tokens = Some(0);

    storage.insert_request_logs(&[unknown, known]).unwrap();

    let rows = rollup_rows(&db_path);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].agent, "agent-a");
    assert_eq!(rows[0].cache_unknown_tokens, 175, "100 + 50 + 25");
    assert_eq!(rows[1].agent, "agent-b");
    assert_eq!(rows[1].cache_unknown_tokens, 0);
}

#[test]
fn replayed_request_id_updates_detail_row_without_double_counting_the_rollup() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let mut first = test_request_record("req-replay");
    first.usage.input_tokens = 100;
    first.usage.output_tokens = 10;
    storage.insert_request_log(&first).unwrap();

    // Same `request_id` replayed with corrected token counts.
    let mut replay = test_request_record("req-replay");
    replay.usage.input_tokens = 400;
    replay.usage.output_tokens = 40;
    storage.insert_request_log(&replay).unwrap();

    // The detail row keeps the latest values, but the rollup is counted once.
    let rows = rollup_rows(&db_path);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].calls, 1, "a replay is not a second call");
    assert_eq!(
        rows[0].input_tokens, 100,
        "the rollup keeps the first value"
    );
    assert_eq!(rows[0].output_tokens, 10);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let details: Vec<(u64, u64)> = {
        let mut stmt = conn
            .prepare("select input_tokens, output_tokens from request_logs")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        details,
        vec![(400, 40)],
        "one detail row with the latest counts"
    );
}

#[test]
fn distinct_request_ids_still_count_as_separate_rollup_calls() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    storage
        .insert_request_logs(&[
            test_request_record("req-distinct-a"),
            test_request_record("req-distinct-b"),
        ])
        .unwrap();

    let rows = rollup_rows(&db_path);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].calls, 2, "two distinct ids are two calls");
    assert_eq!(rows[0].input_tokens, 1122 * 2);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let detail_rows: u64 = conn
        .query_row("select count(*) from request_logs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(detail_rows, 2);
    assert_eq!(
        rows,
        request_log_aggregates(&conn),
        "rollup matches the details"
    );
}

#[test]
fn batch_with_a_duplicate_request_id_in_the_middle_counts_two_rollup_calls() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("any-converter.sqlite3");
    let storage = SqliteStorage::open(&db_path).unwrap();
    let mut first = test_request_record("req-batch-a");
    first.usage.input_tokens = 100;
    let mut duplicate = test_request_record("req-batch-a");
    duplicate.usage.input_tokens = 900;
    let mut third = test_request_record("req-batch-b");
    third.usage.input_tokens = 250;

    storage
        .insert_request_logs(&[first, duplicate, third])
        .unwrap();

    let rows = rollup_rows(&db_path);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].calls, 2,
        "the duplicate in the middle does not add a call"
    );
    assert_eq!(
        rows[0].input_tokens,
        100 + 250,
        "100 is the first write for the dup"
    );

    // `request_logs` is keyed by `request_id`, so the replay collapses into its
    // original row: three inserts, two detail rows, both holding the latest values.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let detail_rows: u64 = conn
        .query_row("select count(*) from request_logs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        detail_rows, 2,
        "the duplicate updates its row instead of adding one"
    );
    let duplicate_tokens: u64 = conn
        .query_row(
            "select input_tokens from request_logs where request_id = 'req-batch-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        duplicate_tokens, 900,
        "the detail keeps the corrected count"
    );
}

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
    priced_calls: u64,
    retried_calls: u64,
    loops: u64,
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
               latency_sum_ms, latency_max_ms, ttft_sum_ms, ttft_calls, cost_usd,
               priced_calls, retried_calls, loops
        from rollup_hourly
        order by bucket, provider, client_model, agent
        "#,
    )
}

fn request_log_aggregates(conn: &rusqlite::Connection) -> Vec<RollupRow> {
    collect_rollup_rows(
        conn,
        r#"
        select substr(timestamp,1,13) || ':00:00Z', provider, client_model, coalesce(client_id,''),
               count(*),
               sum(case when response_status >= 400 then 1 else 0 end),
               sum(input_tokens), sum(output_tokens),
               sum(coalesce(cache_read_tokens,0)), sum(coalesce(cache_write_tokens,0)),
               sum(cache_write_1h),
               sum(case when cache_unknown = 1
                        then input_tokens + coalesce(cache_read_tokens,0) + coalesce(cache_write_tokens,0)
                        else 0 end),
               sum(coalesce(reasoning_tokens,0)),
               sum(latency_ms), max(latency_ms),
               sum(ttft_ms), sum(case when ttft_ms > 0 then 1 else 0 end),
               sum(coalesce(cost_usd,0)),
               sum(case when cost_usd is not null then 1 else 0 end),
               sum(case when attempt > 0 and response_status < 400 then 1 else 0 end),
               sum(case when err_type = 'loop' then 1 else 0 end)
        from request_logs
        group by 1, 2, 3, 4
        order by 1, 2, 3, 4
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
                priced_calls: row.get(18)?,
                retried_calls: row.get(19)?,
                loops: row.get(20)?,
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
