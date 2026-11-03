#![allow(clippy::unwrap_used)]

use any_converter_core::ir::Usage;
use any_converter_server::request_log::{RequestLogRecord, ResponseBodyKind};
use any_converter_server::storage::{
    Granularity, SERIES_KEEP, SplitDim, SqliteStorage, UsageQuery,
};

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
fn usage_summary_keeps_two_providers_in_one_hour_apart() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first = test_request_record("req-provider-a");
    first.provider = "provider-a".to_string();
    first.client_model = "model-a".to_string();
    first.usage.input_tokens = 100;
    first.usage.output_tokens = 10;
    first.usage.cache_read_tokens = Some(40);
    let mut second = test_request_record("req-provider-b");
    second.provider = "provider-b".to_string();
    second.client_model = "model-b".to_string();
    second.usage.input_tokens = 200;
    second.usage.output_tokens = 20;
    second.usage.cache_read_tokens = Some(60);
    storage.insert_request_logs(&[first, second]).unwrap();

    let summary = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();

    // One hour, but two providers: the old min(provider) fold collapsed them.
    assert_eq!(summary.series.len(), 1);
    assert_eq!(summary.totals.calls, 2);
    assert_eq!(summary.totals.input_tokens, 300);
    assert_eq!(summary.totals.output_tokens, 30);
    assert_eq!(summary.totals.cache_read_tokens, 100);

    assert_eq!(summary.by_provider.len(), 2);
    let mut ids: Vec<&str> = summary
        .by_provider
        .iter()
        .map(|group| group.id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["provider-a", "provider-b"]);

    // No token is attributed twice or dropped: the groups add up to the totals.
    assert_eq!(
        summary
            .by_provider
            .iter()
            .map(|group| group.totals.calls)
            .sum::<u64>(),
        summary.totals.calls
    );
    assert_eq!(
        summary
            .by_provider
            .iter()
            .map(|group| group.totals.input_tokens)
            .sum::<u64>(),
        summary.totals.input_tokens
    );
    assert_eq!(
        summary
            .by_provider
            .iter()
            .map(|group| group.totals.output_tokens)
            .sum::<u64>(),
        summary.totals.output_tokens
    );
    assert_eq!(
        summary
            .by_provider
            .iter()
            .map(|group| group.totals.cache_read_tokens)
            .sum::<u64>(),
        summary.totals.cache_read_tokens
    );
    let provider_a = summary
        .by_provider
        .iter()
        .find(|group| group.id == "provider-a")
        .unwrap();
    assert_eq!(provider_a.provider.as_deref(), Some("provider-a"));
    assert_eq!(provider_a.totals.input_tokens, 100);
    assert_eq!(provider_a.totals.cache_read_tokens, 40);
}

#[test]
fn usage_summary_groups_accounts_of_one_provider() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    // One provider, one hour: the rollup folds both calls into a single row, but
    // the account dimension still has to keep them apart.
    let mut first = test_request_record("req-account-a");
    first.provider_account = Some("first@example.com".to_string());
    first.usage.input_tokens = 100;
    first.usage.output_tokens = 10;
    let mut second = test_request_record("req-account-b");
    second.provider_account = Some("second@example.com".to_string());
    second.usage.input_tokens = 400;
    second.usage.output_tokens = 40;
    storage.insert_request_logs(&[first, second]).unwrap();

    let summary = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();

    assert_eq!(summary.by_account.len(), 2);
    assert_eq!(
        summary
            .by_account
            .iter()
            .map(|group| group.totals.calls)
            .sum::<u64>(),
        summary.totals.calls
    );
    // Busiest account first, and each group names the provider that answered it.
    assert_eq!(summary.by_account[0].id, "second@example.com");
    assert_eq!(summary.by_account[0].provider.as_deref(), Some("kimi"));
    assert_eq!(summary.by_account[0].totals.input_tokens, 400);
    assert_eq!(summary.by_account[1].id, "first@example.com");
    assert_eq!(summary.by_account[1].provider.as_deref(), Some("kimi"));
    assert_eq!(summary.by_account[1].totals.input_tokens, 100);
}

#[test]
fn usage_summary_leaves_unattributed_calls_out_of_by_account() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut attributed = test_request_record("req-account-set");
    attributed.provider_account = Some("acct@example.com".to_string());
    let unattributed = test_request_record("req-account-none");
    storage
        .insert_request_logs(&[attributed, unattributed])
        .unwrap();

    let summary = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();

    assert_eq!(
        summary.totals.calls, 2,
        "the unattributed call is still a call"
    );
    assert_eq!(summary.by_account.len(), 1);
    assert_eq!(summary.by_account[0].id, "acct@example.com");
    assert_eq!(summary.by_account[0].totals.calls, 1);
}

#[test]
fn usage_summary_folds_a_session_across_calls() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first = test_request_record("req-session-first");
    first.session_id = Some("session-1".to_string());
    first.client_id = Some("agent-a".to_string());
    first.usage.input_tokens = 100;
    let mut second = test_request_record("req-session-second");
    second.session_id = Some("session-1".to_string());
    second.client_id = Some("agent-a".to_string());
    second.usage.input_tokens = 200;
    let without_session = test_request_record("req-no-session");
    storage
        .insert_request_logs(&[first, second, without_session])
        .unwrap();

    let summary = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();

    assert_eq!(summary.totals.calls, 3);
    assert_eq!(
        summary.by_session.len(),
        1,
        "calls without a session do not group"
    );
    let session = &summary.by_session[0];
    assert_eq!(session.id, "session-1");
    assert_eq!(session.model.as_deref(), Some("agent-a"));
    assert_eq!(session.totals.calls, 2);
    assert_eq!(session.totals.input_tokens, 300);
}

#[test]
fn usage_summary_account_filter_restricts_totals_and_providers() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut selected = test_request_record("req-filter-selected");
    selected.provider_account = Some("a@b.c".to_string());
    selected.usage.input_tokens = 100;
    selected.usage.output_tokens = 10;
    // Same provider and hour as `selected`: a rollup-only account filter would
    // count this row too instead of narrowing to one account.
    let mut same_provider = test_request_record("req-filter-other-kimi");
    same_provider.provider_account = Some("other@b.c".to_string());
    same_provider.usage.input_tokens = 900;
    same_provider.usage.output_tokens = 90;
    let mut other_provider = test_request_record("req-filter-other-moonshot");
    other_provider.provider_account = Some("other@b.c".to_string());
    other_provider.provider = "moonshot".to_string();
    other_provider.client_model = "claude-x".to_string();
    other_provider.usage.input_tokens = 500;
    other_provider.usage.output_tokens = 50;
    storage
        .insert_request_logs(&[selected, same_provider, other_provider])
        .unwrap();

    let query = UsageQuery {
        account: Some("a@b.c".to_string()),
        ..usage_query(Granularity::Hour)
    };
    let summary = storage.usage_summary(&query).unwrap();

    assert_eq!(summary.totals.calls, 1);
    assert_eq!(summary.totals.input_tokens, 100);
    assert_eq!(summary.totals.output_tokens, 10);
    assert_eq!(summary.by_provider.len(), 1);
    assert_eq!(summary.by_provider[0].id, "kimi");
    assert_eq!(summary.by_provider[0].totals.calls, 1);
    assert_eq!(summary.by_account.len(), 1);
    assert_eq!(summary.by_account[0].id, "a@b.c");
    assert_eq!(
        summary
            .series
            .iter()
            .map(|bucket| bucket.totals.calls)
            .sum::<u64>(),
        1
    );
}

#[test]
fn usage_summary_day_granularity_merges_hours_of_the_same_day() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut records = Vec::new();
    for (index, hour) in ["09", "12", "18"].iter().enumerate() {
        let mut record = test_request_record(&format!("req-hour-{index}"));
        record.timestamp = format!("2026-07-16T{hour}:15:00Z");
        record.usage.input_tokens = 100 * (index as u64 + 1);
        record.usage.output_tokens = 10;
        record.usage.cache_read_tokens = Some(5);
        records.push(record);
    }
    storage.insert_request_logs(&records).unwrap();

    // The same three rows bucket into three hours...
    let hourly = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();
    assert_eq!(hourly.series.len(), 3);

    // ...and into exactly one day bucket holding their sum.
    let daily = storage
        .usage_summary(&usage_query(Granularity::Day))
        .unwrap();
    assert_eq!(daily.series.len(), 1);
    let bucket = &daily.series[0];
    assert_eq!(bucket.timestamp, "2026-07-16");
    assert_eq!(bucket.totals.calls, 3);
    assert_eq!(bucket.totals.input_tokens, 600);
    assert_eq!(bucket.totals.output_tokens, 30);
    assert_eq!(bucket.totals.cache_read_tokens, 15);
    assert_eq!(bucket.totals.calls, daily.totals.calls);
    assert_eq!(bucket.totals.input_tokens, daily.totals.input_tokens);
    assert_eq!(bucket.totals.output_tokens, daily.totals.output_tokens);
}

#[test]
fn usage_summary_floors_a_sub_hour_from_to_its_own_bucket() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut at_midnight = test_request_record("req-midnight");
    at_midnight.timestamp = "2026-07-16T00:00:00Z".to_string();
    at_midnight.usage.input_tokens = 100;
    let mut at_noon = test_request_record("req-noon");
    at_noon.timestamp = "2026-07-16T12:00:00Z".to_string();
    at_noon.usage.input_tokens = 200;
    storage
        .insert_request_logs(&[at_midnight, at_noon])
        .unwrap();

    let query = UsageQuery {
        // Half an hour into the first bucket: rounding down must not drop it.
        from: "2026-07-16T00:30:00Z".to_string(),
        to: "2026-07-16T23:00:00Z".to_string(),
        granularity: Granularity::Hour,
        provider: None,
        model: None,
        agent: None,
        account: None,
        split_by: None,
    };
    let summary = storage.usage_summary(&query).unwrap();

    assert_eq!(summary.series.len(), 2);
    assert_eq!(summary.series[0].timestamp, "2026-07-16T00:00:00Z");
    assert_eq!(summary.series[0].totals.input_tokens, 100);
    assert_eq!(summary.series[1].timestamp, "2026-07-16T12:00:00Z");
    assert_eq!(summary.totals.calls, 2);
    assert_eq!(summary.totals.input_tokens, 300);
    // The echoed range is untouched: only the SQL bound is floored.
    assert_eq!(summary.from, "2026-07-16T00:30:00Z");
}

#[test]
fn totals_report_the_calls_that_have_tokens_but_no_price() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut priced = test_request_record("req-priced");
    priced.cost_usd = Some(0.5);
    // Two calls the pricing table never covered: their tokens are real, their
    // price is unknown, so they must not read as free traffic.
    let unpriced_a = test_request_record("req-unpriced-a");
    let unpriced_b = test_request_record("req-unpriced-b");
    storage
        .insert_request_logs(&[priced, unpriced_a, unpriced_b])
        .unwrap();

    let summary = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();
    assert_eq!(summary.totals.calls, 3);
    assert_eq!(summary.totals.cost_usd, 0.5);
    assert_eq!(
        summary.totals.unpriced, 2,
        "two calls reported tokens with no known price"
    );

    // Every fold of the same rows carries the count, not just the grand total.
    assert_eq!(summary.series[0].totals.unpriced, 2);
    assert_eq!(summary.by_provider[0].totals.unpriced, 2);
    assert_eq!(summary.by_model[0].totals.unpriced, 2);
}

#[test]
fn totals_report_nothing_unpriced_when_every_call_has_a_price() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut records = Vec::new();
    for (request_id, cost) in [("req-p1", 0.25), ("req-p2", 0.75), ("req-p3", 2.0)] {
        let mut record = test_request_record(request_id);
        record.cost_usd = Some(cost);
        records.push(record);
    }
    storage.insert_request_logs(&records).unwrap();

    let totals = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(totals.calls, 3);
    assert_eq!(totals.cost_usd, 3.0);
    assert_eq!(totals.unpriced, 0, "every call was priced");
}

#[test]
fn usage_summary_counts_a_success_answered_after_a_retry() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first_try = test_request_record("req-first-try");
    first_try.attempt = 0;
    let mut retried = test_request_record("req-retried");
    retried.attempt = 2;
    retried.response_status = 200;
    storage.insert_request_logs(&[first_try, retried]).unwrap();

    let totals = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(totals.calls, 2);
    assert_eq!(totals.errors, 0);
    assert_eq!(
        totals.retried, 1,
        "only the call that answered after a failed try counts"
    );
    assert_eq!(totals.loops, 0);
}

#[test]
fn usage_summary_reports_the_decode_speed_of_the_rolled_up_calls() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first = test_request_record("req-speed-a");
    first.decode_ms = 1000;
    first.decode_out = 500;
    let mut second = test_request_record("req-speed-b");
    second.decode_ms = 3000;
    second.decode_out = 1500;
    storage.insert_request_logs(&[first, second]).unwrap();

    let totals = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(totals.calls, 2);
    assert_eq!(totals.decode_ms, 4000);
    assert_eq!(totals.decode_out, 2000);
    assert_eq!(totals.decode_n, 2);
    // 2000 tokens over 4 seconds: the summary reports the speed the ledger rows carry.
    assert_eq!(totals.speed(), Some(500.0));
}

#[test]
fn decode_window_with_no_output_does_not_count_as_a_measured_reply() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first = test_request_record("req-speed-a");
    first.decode_ms = 1000;
    first.decode_out = 500;
    let mut second = test_request_record("req-speed-b");
    second.decode_ms = 3000;
    second.decode_out = 1500;
    // A window that produced nothing still spent its milliseconds, so it belongs
    // in the denominator's sum but not in the count of measured replies.
    let mut empty = test_request_record("req-speed-empty");
    empty.decode_ms = 1000;
    empty.decode_out = 0;
    storage
        .insert_request_logs(&[first, second, empty])
        .unwrap();

    let totals = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(
        totals.decode_ms, 5000,
        "the empty window's time is included"
    );
    assert_eq!(totals.decode_out, 2000);
    assert_eq!(totals.decode_n, 2, "only the two replies wrote tokens");
    assert_eq!(totals.speed(), Some(400.0));
}

#[test]
fn rebuild_rollup_reproduces_the_decode_speed() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first = test_request_record("req-speed-a");
    first.decode_ms = 1000;
    first.decode_out = 500;
    let mut second = test_request_record("req-speed-b");
    second.decode_ms = 3000;
    second.decode_out = 1500;
    storage.insert_request_logs(&[first, second]).unwrap();

    // The rebuild recomputes the rollup from `request_logs` and must land on the
    // same three sums the write path folded, or old traffic loses its speed.
    storage.rebuild_rollup().unwrap();

    let totals = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap()
        .totals;
    assert_eq!(totals.decode_ms, 4000);
    assert_eq!(totals.decode_out, 2000);
    assert_eq!(totals.decode_n, 2);
    assert_eq!(totals.speed(), Some(500.0));
}

#[test]
fn usage_summary_splits_each_point_by_provider() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    // Three providers over two hours, so every point has all three lines to stack.
    let mut records = Vec::new();
    for (hour_index, hour) in ["09", "10"].iter().enumerate() {
        for (provider_index, provider) in ["provider-a", "provider-b", "provider-c"]
            .iter()
            .enumerate()
        {
            let mut record = test_request_record(&format!("req-{provider}-{hour}"));
            record.timestamp = format!("2026-07-16T{hour}:05:00Z");
            record.provider = (*provider).to_string();
            record.usage.input_tokens = 100 * (hour_index as u64 + 1) + provider_index as u64;
            record.usage.output_tokens = 10 * (provider_index as u64 + 1);
            record.usage.cache_read_tokens = Some(7);
            record.usage.cache_write_tokens = Some(3);
            records.push(record);
        }
    }
    storage.insert_request_logs(&records).unwrap();

    let query = UsageQuery {
        split_by: Some(SplitDim::Provider),
        ..usage_query(Granularity::Hour)
    };
    let summary = storage.usage_summary(&query).unwrap();

    assert_eq!(summary.series.len(), 2);
    for bucket in &summary.series {
        let by = bucket
            .by
            .as_ref()
            .expect("a split was asked for, so every point has one");
        assert_eq!(by.len(), 3, "{} has a line per provider", bucket.timestamp);

        // The chart counts cache traffic too; the stack is the point, split up.
        let stacked: u64 = by
            .values()
            .map(|totals| {
                totals.input_tokens
                    + totals.output_tokens
                    + totals.cache_read_tokens
                    + totals.cache_write_tokens
            })
            .sum();
        let point = bucket.totals.input_tokens
            + bucket.totals.output_tokens
            + bucket.totals.cache_read_tokens
            + bucket.totals.cache_write_tokens;
        assert_eq!(stacked, point, "no token is dropped or counted twice");
        for provider in ["provider-a", "provider-b", "provider-c"] {
            assert_eq!(by[provider].calls, 1, "{provider} called once per hour");
        }
    }
}

#[test]
fn usage_summary_keeps_the_busiest_split_values_only() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    // Thirty providers in one hour: the chart cannot draw them all.
    let mut records = Vec::new();
    for index in 0..30u64 {
        let mut record = test_request_record(&format!("req-split-{index}"));
        record.provider = format!("provider-{index:02}");
        record.usage.input_tokens = 100 + index;
        record.usage.output_tokens = 10;
        records.push(record);
    }
    storage.insert_request_logs(&records).unwrap();

    let query = UsageQuery {
        split_by: Some(SplitDim::Provider),
        ..usage_query(Granularity::Hour)
    };
    let summary = storage.usage_summary(&query).unwrap();

    assert_eq!(summary.totals.calls, 30, "the tail is still traffic");
    assert_eq!(summary.series.len(), 1);
    let bucket = &summary.series[0];
    let by = bucket
        .by
        .as_ref()
        .expect("some values survive, so the point is split");
    assert_eq!(by.len(), SERIES_KEEP, "at most the keep survives");

    // Tokens rise with the index, so 06..=29 are the busiest and 00..=05 the
    // tail that folded back into the point's own totals.
    for index in 0..30u64 {
        let provider = format!("provider-{index:02}");
        assert_eq!(by.contains_key(&provider), index >= 6, "{provider}");
    }
    assert_eq!(
        by.values().map(|totals| totals.calls).sum::<u64>(),
        SERIES_KEEP as u64
    );
    assert_eq!(bucket.totals.calls, 30);
}

#[test]
fn usage_summary_does_not_split_without_being_asked() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first = test_request_record("req-nosplit-a");
    first.provider = "provider-a".to_string();
    let mut second = test_request_record("req-nosplit-b");
    second.provider = "provider-b".to_string();
    storage.insert_request_logs(&[first, second]).unwrap();

    let summary = storage
        .usage_summary(&usage_query(Granularity::Hour))
        .unwrap();

    assert!(!summary.series.is_empty());
    assert!(
        summary.series.iter().all(|bucket| bucket.by.is_none()),
        "no split was asked for, so no point carries one"
    );
}

#[test]
fn usage_summary_cannot_split_by_account() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();

    let mut first = test_request_record("req-split-account-a");
    first.provider_account = Some("first@example.com".to_string());
    let mut second = test_request_record("req-split-account-b");
    second.provider_account = Some("second@example.com".to_string());
    storage.insert_request_logs(&[first, second]).unwrap();

    let query = UsageQuery {
        split_by: Some(SplitDim::Account),
        ..usage_query(Granularity::Hour)
    };
    let summary = storage.usage_summary(&query).unwrap();

    assert!(!summary.series.is_empty());
    assert!(
        summary.series.iter().all(|bucket| bucket.by.is_none()),
        "the account split is not pre-aggregated on the rollup"
    );
    // The ranking that does have the detail column still answers the question.
    assert_eq!(summary.by_account.len(), 2);
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
