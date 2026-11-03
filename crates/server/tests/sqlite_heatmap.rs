#![allow(clippy::unwrap_used)]

use any_converter_core::ir::Usage;
use any_converter_server::request_log::{RequestLogRecord, ResponseBodyKind};
use any_converter_server::storage::{Granularity, HEATMAP_WEEKS, SqliteStorage, UsageQuery};
use chrono::Datelike;

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

/// A record stamped `timestamp`, which the rollup folds into its hour bucket.
fn record_at(request_id: &str, timestamp: chrono::DateTime<chrono::Utc>) -> RequestLogRecord {
    let mut record = test_request_record(request_id);
    record.timestamp = timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    record
}

#[test]
fn heatmap_keeps_today_and_drops_a_call_400_days_back() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    let now = chrono::Utc::now();

    let mut today = record_at("req-heat-today", now);
    today.usage.input_tokens = 100;
    today.usage.output_tokens = 20;
    today.usage.cache_read_tokens = Some(7);
    today.usage.cache_write_tokens = Some(3);
    let mut ancient = record_at("req-heat-ancient", now - chrono::Duration::days(400));
    ancient.usage.input_tokens = 9_000;
    ancient.usage.output_tokens = 900;
    storage.insert_request_logs(&[today, ancient]).unwrap();

    // The query's own range is deliberately elsewhere: the heatmap has its own span.
    let heatmap = storage.heatmap(&usage_query(Granularity::Day)).unwrap();

    assert_eq!(
        heatmap.days.len(),
        1,
        "400 days back is outside the 53 weeks"
    );
    let day = &heatmap.days[0];
    assert_eq!(day.date, now.date_naive().to_string());
    assert_eq!(day.calls, 1);
    // The chart counts cache traffic too: 100 + 20 + 7 + 3.
    assert_eq!(day.tokens, 130);
}

#[test]
fn heatmap_merges_two_calls_of_one_day_into_one_cell() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    // Yesterday is fully elapsed, so neither hour can fall past the window's end.
    let day = chrono::Utc::now().date_naive() - chrono::Duration::days(1);
    let at = |hour| day.and_hms_opt(hour, 0, 0).unwrap().and_utc();

    let mut morning = record_at("req-heat-morning", at(1));
    morning.usage.input_tokens = 100;
    morning.usage.output_tokens = 10;
    morning.usage.cache_read_tokens = Some(5);
    morning.usage.cache_write_tokens = None;
    morning.cost_usd = Some(0.25);
    let mut evening = record_at("req-heat-evening", at(9));
    evening.usage.input_tokens = 200;
    evening.usage.output_tokens = 20;
    evening.usage.cache_read_tokens = None;
    evening.usage.cache_write_tokens = Some(7);
    evening.cost_usd = Some(0.5);
    storage.insert_request_logs(&[morning, evening]).unwrap();

    let heatmap = storage.heatmap(&usage_query(Granularity::Day)).unwrap();

    // Two different hours, one day: two rollup rows, one cell.
    assert_eq!(heatmap.days.len(), 1);
    let cell = &heatmap.days[0];
    assert_eq!(cell.date, day.to_string());
    assert_eq!(cell.calls, 2);
    assert_eq!(cell.tokens, 100 + 10 + 5 + 200 + 20 + 7);
    assert_eq!(cell.unpriced, 0);
    assert!((cell.cost_usd - 0.75).abs() < 1e-9, "{}", cell.cost_usd);
}

#[test]
fn heatmap_is_53_weeks_starting_on_a_monday() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    let mut today = record_at("req-heat-shot", chrono::Utc::now());
    today.usage.input_tokens = 10;
    storage.insert_request_logs(&[today]).unwrap();

    let heatmap = storage.heatmap(&usage_query(Granularity::Day)).unwrap();

    assert_eq!(heatmap.weeks, HEATMAP_WEEKS);
    assert_eq!(heatmap.weeks, 53);
    let from = chrono::NaiveDate::parse_from_str(&heatmap.from, "%Y-%m-%d").unwrap();
    let to = chrono::NaiveDate::parse_from_str(&heatmap.to, "%Y-%m-%d").unwrap();
    assert_eq!(from.weekday(), chrono::Weekday::Mon);
    let to_monday = to - chrono::Duration::days(i64::from(to.weekday().num_days_from_monday()));
    assert_eq!(from, to_monday - chrono::Duration::weeks(52));
}

#[test]
fn heatmap_counts_the_calls_with_no_price() {
    let temp = tempfile::tempdir().unwrap();
    let storage = SqliteStorage::open(temp.path().join("any-converter.sqlite3")).unwrap();
    let now = chrono::Utc::now();

    let mut priced = record_at("req-heat-priced", now);
    priced.usage.input_tokens = 10;
    priced.cost_usd = Some(0.5);
    let mut first_unpriced = record_at("req-heat-unpriced-a", now - chrono::Duration::hours(1));
    first_unpriced.usage.input_tokens = 20;
    first_unpriced.cost_usd = None;
    let mut second_unpriced = record_at("req-heat-unpriced-b", now - chrono::Duration::hours(2));
    second_unpriced.usage.input_tokens = 30;
    second_unpriced.cost_usd = None;
    storage
        .insert_request_logs(&[priced, first_unpriced, second_unpriced])
        .unwrap();

    let heatmap = storage.heatmap(&usage_query(Granularity::Day)).unwrap();

    // The three hours can straddle midnight, in which case the cells split; every
    // call still lands in exactly one of them and is counted once.
    let calls: u64 = heatmap.days.iter().map(|day| day.calls).sum();
    let unpriced: u64 = heatmap.days.iter().map(|day| day.unpriced).sum();
    let cost: f64 = heatmap.days.iter().map(|day| day.cost_usd).sum();
    assert_eq!(calls, 3);
    assert_eq!(unpriced, 2);
    assert!((cost - 0.5).abs() < 1e-9, "{cost}");
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
