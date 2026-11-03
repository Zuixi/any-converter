#![allow(clippy::unwrap_used)]

use std::collections::HashSet;

use any_converter_core::ir::Usage;
use any_converter_server::request_log::{RequestLogRecord, ResponseBodyKind};
use any_converter_server::storage::{
    LedgerFilter, LedgerPage, LedgerRow, Share, SqliteStorage, Totals,
};

fn open() -> (tempfile::TempDir, SqliteStorage) {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    (temp, storage)
}

/// A window wide enough to hold every fixture row, so only the field under test
/// can narrow the result.
fn filter() -> LedgerFilter {
    LedgerFilter {
        from: "2026-07-01T00:00:00Z".to_string(),
        to: "2026-08-01T00:00:00Z".to_string(),
        ..LedgerFilter::default()
    }
}

fn collect(storage: &SqliteStorage, filter: &LedgerFilter) -> LedgerPage {
    storage.ledger_page(filter, None, 500).unwrap()
}

fn ids(page: &LedgerPage) -> Vec<String> {
    page.rows.iter().map(|row| row.request_id.clone()).collect()
}

fn sorted_ids(page: &LedgerPage) -> Vec<String> {
    let mut ids = ids(page);
    ids.sort_unstable();
    ids
}

#[test]
fn ledger_pages_250_rows_at_100_per_page_without_gaps_or_duplicates() {
    let (_temp, storage) = open();
    let records: Vec<_> = (0..250)
        .map(|index| test_record(&format!("req-{index:03}")))
        .collect();
    storage.insert_request_logs(&records).unwrap();

    let the_filter = filter();
    let mut seen = Vec::new();
    let mut sizes = Vec::new();
    let mut cursor = None;
    loop {
        let page = storage.ledger_page(&the_filter, cursor, 100).unwrap();
        // The total describes the filter, not the page: it never shrinks as we page.
        assert_eq!(page.total, 250);
        sizes.push(page.rows.len());
        seen.extend(ids(&page));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(sizes, vec![100, 100, 50]);
    assert_eq!(seen.len(), 250, "no row was skipped");
    assert_eq!(
        seen.iter().collect::<HashSet<_>>().len(),
        250,
        "no row came back twice"
    );
    // Newest first, and every id in between is present exactly once.
    let expected: Vec<String> = (0..250)
        .rev()
        .map(|index| format!("req-{index:03}"))
        .collect();
    assert_eq!(seen, expected);

    // A limit above the cap is clamped to 500, still one short page of 250.
    let capped = storage.ledger_page(&the_filter, None, 10_000).unwrap();
    assert_eq!(capped.rows.len(), 250);
    assert_eq!(capped.next_cursor, None);

    // A limit of zero falls back to the default page size.
    let defaulted = storage.ledger_page(&the_filter, None, 0).unwrap();
    assert_eq!(defaulted.rows.len(), 100);
    assert_eq!(defaulted.total, 250);
}

#[test]
fn ledger_failed_filter_keeps_only_error_statuses() {
    let (_temp, storage) = open();
    let mut records = Vec::new();
    for (request_id, status) in [
        ("req-ok", 200),
        ("req-bad-request", 400),
        ("req-rate-limited", 429),
        ("req-server-error", 500),
    ] {
        let mut record = test_record(request_id);
        record.response_status = status;
        records.push(record);
    }
    storage.insert_request_logs(&records).unwrap();

    let the_filter = LedgerFilter {
        failed: true,
        ..filter()
    };
    let page = collect(&storage, &the_filter);
    assert_eq!(
        sorted_ids(&page),
        vec!["req-bad-request", "req-rate-limited", "req-server-error"]
    );
    assert!(page.rows.iter().all(|row| row.status >= 400));
    assert_eq!(page.total, 3);
    assert_eq!(page.totals.calls, 3);
    assert_eq!(page.totals.errors, 3);

    // Without the flag, every row is kept.
    let all = collect(&storage, &filter());
    assert_eq!(all.total, 4);
    assert_eq!(all.totals.errors, 3);
}

#[test]
fn ledger_retried_filter_keeps_only_rows_answered_after_a_failed_try() {
    let (_temp, storage) = open();
    let first_try = test_record("req-first-try");
    let mut retried = test_record("req-retried");
    retried.attempt = 2;
    let mut retried_failed = test_record("req-retried-failed");
    retried_failed.attempt = 1;
    retried_failed.response_status = 500;
    storage
        .insert_request_logs(&[first_try, retried, retried_failed])
        .unwrap();

    let page = collect(
        &storage,
        &LedgerFilter {
            retried: true,
            ..filter()
        },
    );
    assert_eq!(
        sorted_ids(&page),
        vec!["req-retried", "req-retried-failed"],
        "the filter keeps every row that needed more than one try"
    );
    assert!(page.rows.iter().all(|row| row.attempt > 0));
    assert_eq!(page.total, 2);
    // `Retried` is a success that answered after a try failed first, so the
    // failed row is kept by the filter but is not counted here.
    assert_eq!(page.totals.retried, 1);
}

#[test]
fn ledger_row_reports_the_attempt_that_answered() {
    let (_temp, storage) = open();
    let mut record = test_record("req-fourth-try");
    record.attempt = 3;
    storage.insert_request_log(&record).unwrap();

    let page = collect(&storage, &filter());
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].attempt, 3);
    assert_eq!(page.totals.retried, 1);
}

#[test]
fn ledger_equality_filters_narrow_provider_model_agent_and_account() {
    let (_temp, storage) = open();
    let mut first = test_record("req-moonshot-a");
    first.provider = "moonshot".to_string();
    first.client_model = "gpt-5.4".to_string();
    first.client_id = Some("agent-a".to_string());
    first.provider_account = Some("acct-1".to_string());

    let mut second = test_record("req-kimi-b");
    second.provider = "kimi".to_string();
    second.client_model = "claude-opus-5".to_string();
    second.client_id = Some("agent-b".to_string());
    second.provider_account = Some("acct-2".to_string());

    let mut third = test_record("req-moonshot-c");
    third.provider = "moonshot".to_string();
    third.client_model = "gpt-5.4".to_string();
    third.client_id = None;
    third.provider_account = None;

    storage
        .insert_request_logs(&[first, second, third])
        .unwrap();

    let by_provider = collect(
        &storage,
        &LedgerFilter {
            provider: Some("moonshot".to_string()),
            ..filter()
        },
    );
    assert_eq!(
        sorted_ids(&by_provider),
        vec!["req-moonshot-a", "req-moonshot-c"]
    );
    assert!(
        by_provider
            .rows
            .iter()
            .all(|row| row.provider == "moonshot")
    );

    let by_model = collect(
        &storage,
        &LedgerFilter {
            model: Some("claude-opus-5".to_string()),
            ..filter()
        },
    );
    assert_eq!(sorted_ids(&by_model), vec!["req-kimi-b"]);
    assert_eq!(by_model.rows[0].client_model, "claude-opus-5");

    let by_agent = collect(
        &storage,
        &LedgerFilter {
            agent: Some("agent-a".to_string()),
            ..filter()
        },
    );
    assert_eq!(sorted_ids(&by_agent), vec!["req-moonshot-a"]);
    assert_eq!(by_agent.rows[0].agent.as_deref(), Some("agent-a"));

    let by_account = collect(
        &storage,
        &LedgerFilter {
            account: Some("acct-2".to_string()),
            ..filter()
        },
    );
    assert_eq!(sorted_ids(&by_account), vec!["req-kimi-b"]);
    assert_eq!(by_account.rows[0].account.as_deref(), Some("acct-2"));

    // Filters compose: two equalities must both hold.
    let combined = collect(
        &storage,
        &LedgerFilter {
            provider: Some("moonshot".to_string()),
            model: Some("gpt-5.4".to_string()),
            agent: Some("agent-a".to_string()),
            ..filter()
        },
    );
    assert_eq!(sorted_ids(&combined), vec!["req-moonshot-a"]);
    assert_eq!(combined.total, 1);
}

#[test]
fn ledger_query_matches_case_insensitively_across_model_and_provider() {
    let (_temp, storage) = open();
    let mut by_model = test_record("req-by-model");
    by_model.provider = "kimi".to_string();
    by_model.client_model = "Claude-Sonnet-5".to_string();

    let mut by_provider = test_record("req-by-provider");
    by_provider.provider = "claude-gateway".to_string();
    by_provider.client_model = "gpt-5.4".to_string();

    let mut unrelated = test_record("req-unrelated");
    unrelated.provider = "moonshot".to_string();
    unrelated.client_model = "gpt-5.4".to_string();

    storage
        .insert_request_logs(&[by_model, by_provider, unrelated])
        .unwrap();

    let page = collect(
        &storage,
        &LedgerFilter {
            query: Some("CLAUDE".to_string()),
            ..filter()
        },
    );
    assert_eq!(
        sorted_ids(&page),
        vec!["req-by-model", "req-by-provider"],
        "the needle is lowercased and matched against model and provider"
    );
    assert_eq!(page.total, 2);

    let none = collect(
        &storage,
        &LedgerFilter {
            query: Some("no-such-model".to_string()),
            ..filter()
        },
    );
    assert!(none.rows.is_empty());
    assert_eq!(none.total, 0);
    assert_eq!(none.totals.calls, 0);
    assert_eq!(none.totals.cost_usd, 0.0);
}

#[test]
fn ledger_day_filter_keeps_only_that_local_day() {
    let (_temp, storage) = open();
    let mut first = test_record("req-morning");
    first.timestamp = "2026-07-16T09:00:00Z".to_string();
    let mut second = test_record("req-late");
    second.timestamp = "2026-07-16T23:59:00Z".to_string();
    let mut third = test_record("req-next-day");
    third.timestamp = "2026-07-17T00:01:00Z".to_string();
    storage
        .insert_request_logs(&[first, second, third])
        .unwrap();

    let page = collect(
        &storage,
        &LedgerFilter {
            day: Some("2026-07-16".to_string()),
            ..filter()
        },
    );
    assert_eq!(sorted_ids(&page), vec!["req-late", "req-morning"]);
    assert!(
        page.rows
            .iter()
            .all(|row| row.timestamp.starts_with("2026-07-16"))
    );
    assert_eq!(page.total, 2);

    // The day narrows the range; it does not replace it.
    let outside_range = collect(
        &storage,
        &LedgerFilter {
            from: "2026-07-17T00:00:00Z".to_string(),
            day: Some("2026-07-16".to_string()),
            ..filter()
        },
    );
    assert!(outside_range.rows.is_empty());
}

#[test]
fn ledger_totals_equal_the_sum_over_the_rows_of_one_page() {
    let (_temp, storage) = open();
    let mut records = Vec::new();
    for index in 0..4_u64 {
        let mut record = test_record(&format!("req-totals-{index}"));
        record.response_status = if index % 2 == 0 { 200 } else { 500 };
        record.usage.input_tokens = 100 * (index + 1);
        record.usage.output_tokens = 10 * (index + 1);
        record.usage.cache_read_tokens = Some(5 * (index + 1));
        record.usage.cache_write_tokens = Some(index);
        record.usage.cache_write_1h = Some(7 * (index + 1));
        record.usage.reasoning_tokens = Some(index);
        record.latency_ms = 1000 + index;
        record.ttft_ms = if index == 0 { 0 } else { 100 * index };
        record.cost_usd = Some(0.25 * (index as f64 + 1.0));
        // Only row 1 counted no cache: its prompt tokens leave the hit-rate denominator.
        record.cache_unknown = index == 1;
        records.push(record);
    }
    storage.insert_request_logs(&records).unwrap();

    let page = collect(&storage, &filter());
    assert_eq!(page.rows.len(), 4);
    assert_eq!(page.total, 4);
    assert_eq!(page.next_cursor, None);

    let summed = sum_rows(&page.rows);
    assert_eq!(page.totals.calls, 4);
    assert_eq!(page.totals.calls, page.rows.len() as u64);
    assert_eq!(page.totals.errors, 2);
    assert_eq!(page.totals.input_tokens, summed.input_tokens);
    assert_eq!(page.totals.output_tokens, summed.output_tokens);
    assert_eq!(page.totals.cache_read_tokens, summed.cache_read_tokens);
    assert_eq!(page.totals.cache_write_tokens, summed.cache_write_tokens);
    assert_eq!(page.totals.reasoning_tokens, summed.reasoning_tokens);

    // The same numbers the rows carry, spelled out.
    assert_eq!(page.totals.input_tokens, 100 + 200 + 300 + 400);
    assert_eq!(page.totals.output_tokens, 10 + 20 + 30 + 40);
    assert_eq!(page.totals.cache_read_tokens, 5 + 10 + 15 + 20);
    assert_eq!(page.totals.cache_write_tokens, 1 + 2 + 3);
    assert_eq!(page.totals.cache_write_1h, 7 + 14 + 21 + 28);
    assert_eq!(page.totals.cache_unknown_tokens, 200 + 10 + 1);
    assert_eq!(page.totals.reasoning_tokens, 1 + 2 + 3);
    assert_eq!(page.totals.latency_sum_ms, 1000 + 1001 + 1002 + 1003);
    assert_eq!(page.totals.latency_max_ms, 1003);
    assert_eq!(page.totals.ttft_sum_ms, 100 + 200 + 300);
    assert_eq!(
        page.totals.ttft_calls, 3,
        "the zero ttft call does not count"
    );
    assert_eq!(page.totals.cost_usd, 0.25 + 0.5 + 0.75 + 1.0);

    // Totals describe the whole filtered set, not the page on screen.
    let first_page = storage.ledger_page(&filter(), None, 2).unwrap();
    assert_eq!(first_page.rows.len(), 2);
    assert!(first_page.next_cursor.is_some());
    assert_eq!(first_page.total, 4);
    assert_eq!(first_page.totals.calls, 4);
    assert_eq!(first_page.totals.input_tokens, page.totals.input_tokens);
}

fn ids_of(shares: &[Share]) -> Vec<&str> {
    shares.iter().map(|share| share.id.as_str()).collect()
}

/// A record with the three switchable dimensions spelled out.
fn record(request_id: &str, provider: &str, model: &str, agent: Option<&str>) -> RequestLogRecord {
    let mut record = test_record(request_id);
    record.provider = provider.to_string();
    record.client_model = model.to_string();
    record.client_id = agent.map(str::to_string);
    record
}

#[test]
fn ledger_by_clears_only_the_dimension_it_ranks() {
    let (_temp, storage) = open();
    storage
        .insert_request_logs(&[
            record("req-kimi-gpt", "kimi", "gpt-5.4", Some("claude-code")),
            record(
                "req-kimi-opus",
                "kimi",
                "claude-opus-5",
                Some("claude-code"),
            ),
            record("req-moonshot-gpt", "moonshot", "gpt-5.4", Some("codex")),
            record("req-bedrock-opus", "bedrock", "claude-opus-5", None),
        ])
        .unwrap();

    let by = storage
        .ledger_by(&LedgerFilter {
            provider: Some("kimi".to_string()),
            ..filter()
        })
        .unwrap();

    // The provider ranking ignores its own filter, so every provider is still an
    // option to switch back to, and none of them is a zero-call leftover.
    assert_eq!(ids_of(&by.provider), vec!["kimi", "bedrock", "moonshot"]);
    assert!(by.provider.iter().all(|share| share.totals.calls > 0));
    assert!(
        by.provider
            .iter()
            .any(|share| share.id == "moonshot" && share.totals.calls == 1)
    );
    assert!(by.provider.iter().all(|share| share.label == share.id));
    assert!(by.provider.iter().all(|share| share.provider.is_none()));
    let kimi = &by.provider[0];
    assert_eq!(kimi.totals.calls, 2);
    assert_eq!(kimi.totals.input_tokens, 200);
    assert_eq!(kimi.totals.latency_sum_ms, 2468);

    // The model and agent rankings still honor the provider filter.
    assert_eq!(ids_of(&by.model), vec!["claude-opus-5", "gpt-5.4"]);
    assert!(
        by.model
            .iter()
            .all(|share| share.provider.as_deref() == Some("kimi"))
    );
    assert!(
        by.model
            .iter()
            .all(|share| share.label.ends_with(" · kimi"))
    );
    assert_eq!(ids_of(&by.agent), vec!["claude-code"]);
}

#[test]
fn ledger_by_agent_keeps_the_other_agents_selectable() {
    let (_temp, storage) = open();
    storage
        .insert_request_logs(&[
            record("req-agent-a-1", "kimi", "gpt-5.4", Some("claude-code")),
            record("req-agent-a-2", "kimi", "gpt-5.4", Some("claude-code")),
            record("req-agent-b", "moonshot", "gpt-5.4", Some("codex")),
            record("req-agent-none", "moonshot", "gpt-5.4", None),
        ])
        .unwrap();

    let by = storage
        .ledger_by(&LedgerFilter {
            agent: Some("claude-code".to_string()),
            ..filter()
        })
        .unwrap();

    // The agent ranking ignores its own filter, so the other agent still shows up.
    assert_eq!(ids_of(&by.agent), vec!["claude-code", "codex"]);
    assert!(by.agent.iter().all(|share| !share.id.is_empty()));
    assert_eq!(by.agent[0].totals.calls, 2);

    // Everything else keeps the agent filter: only that agent's provider.
    assert_eq!(ids_of(&by.provider), vec!["kimi"]);
    assert_eq!(ids_of(&by.model), vec!["gpt-5.4"]);
}

#[test]
fn ledger_by_model_labels_name_the_provider_and_keep_same_names_apart() {
    let (_temp, storage) = open();
    storage
        .insert_request_logs(&[
            record("req-kimi-gpt", "kimi", "gpt-5.4", None),
            record("req-moonshot-gpt", "moonshot", "gpt-5.4", None),
        ])
        .unwrap();

    let by = storage.ledger_by(&filter()).unwrap();

    // Same model name, two routes: two entries, each naming its provider.
    assert_eq!(ids_of(&by.model), vec!["gpt-5.4", "gpt-5.4"]);
    let mut labels: Vec<&str> = by.model.iter().map(|share| share.label.as_str()).collect();
    labels.sort_unstable();
    assert_eq!(labels, vec!["gpt-5.4 · kimi", "gpt-5.4 · moonshot"]);
    let mut providers: Vec<&str> = by
        .model
        .iter()
        .filter_map(|share| share.provider.as_deref())
        .collect();
    providers.sort_unstable();
    assert_eq!(providers, vec!["kimi", "moonshot"]);
    assert!(by.model.iter().all(|share| share.totals.calls == 1));
}

#[test]
fn ledger_by_on_an_empty_range_returns_three_empty_rankings() {
    let (_temp, storage) = open();

    let nothing_ingested = storage.ledger_by(&filter()).unwrap();
    assert!(nothing_ingested.provider.is_empty());
    assert!(nothing_ingested.agent.is_empty());
    assert!(nothing_ingested.model.is_empty());

    storage
        .insert_request_logs(&[record("req-out-of-range", "kimi", "gpt-5.4", Some("codex"))])
        .unwrap();

    let outside = storage
        .ledger_by(&LedgerFilter {
            from: "2027-01-01T00:00:00Z".to_string(),
            to: "2027-02-01T00:00:00Z".to_string(),
            ..LedgerFilter::default()
        })
        .unwrap();
    assert!(outside.provider.is_empty());
    assert!(outside.agent.is_empty());
    assert!(outside.model.is_empty());
}

fn sum_rows(rows: &[LedgerRow]) -> Totals {
    let mut totals = Totals::default();
    for row in rows {
        totals.calls += 1;
        totals.errors += u64::from(row.status >= 400);
        totals.input_tokens += row.input_tokens;
        totals.output_tokens += row.output_tokens;
        totals.cache_read_tokens += row.cache_read_tokens;
        totals.cache_write_tokens += row.cache_write_tokens;
        totals.reasoning_tokens += row.reasoning_tokens;
        totals.latency_sum_ms += row.latency_ms;
        totals.latency_max_ms = totals.latency_max_ms.max(row.latency_ms);
        totals.ttft_sum_ms += row.ttft_ms;
        totals.ttft_calls += u64::from(row.ttft_ms > 0);
        totals.cost_usd += row.cost_usd.unwrap_or(0.0);
    }
    totals
}

fn test_record(request_id: &str) -> RequestLogRecord {
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
        response_body: ResponseBodyKind::Json {
            text: "{}".to_string(),
        },
        latency_ms: 1234,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_tokens: Some(5),
            cache_write_tokens: Some(2),
            cache_write_1h: None,
            reasoning_tokens: Some(1),
        },
        trace: None,
        truncated: false,
    }
}
