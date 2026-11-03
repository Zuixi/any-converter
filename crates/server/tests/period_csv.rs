#![allow(clippy::unwrap_used)]

use any_converter_server::ledger_csv;
use any_converter_server::period::{PeriodError, resolve};
use any_converter_server::storage::LedgerRow;
use chrono::{Duration, TimeZone, Utc};

#[test]
fn period_7d_ends_now_and_starts_seven_days_earlier() {
    let now = Utc.with_ymd_and_hms(2026, 7, 16, 13, 45, 12).unwrap();
    let range = resolve("7d", now).unwrap();

    assert_eq!(range.to, "2026-07-16T13:45:12Z");
    assert_eq!(range.from, "2026-07-09T13:45:12Z");
    assert!(range.from.ends_with('Z'));
    assert!(range.to.ends_with('Z'));
    assert_eq!(range.from.len(), "2026-07-09T13:45:12Z".len());
    assert_eq!(range.to.len(), "2026-07-16T13:45:12Z".len());
}

#[test]
fn period_today_is_this_utc_calendar_day() {
    let now = Utc.with_ymd_and_hms(2026, 7, 16, 13, 45, 12).unwrap();
    let range = resolve("today", now).unwrap();

    assert_eq!(range.from, "2026-07-16T00:00:00Z");
    assert_eq!(range.to, "2026-07-17T00:00:00Z");
}

#[test]
fn period_last_90m_is_a_ninety_minute_window_ending_now() {
    let now = Utc.with_ymd_and_hms(2026, 7, 16, 13, 45, 12).unwrap();
    let range = resolve("last:90m", now).unwrap();

    let from = chrono::DateTime::parse_from_rfc3339(&range.from).unwrap();
    let to = chrono::DateTime::parse_from_rfc3339(&range.to).unwrap();
    assert_eq!(to - from, Duration::minutes(90));
    assert_eq!(range.to, "2026-07-16T13:45:12Z");
    assert_eq!(range.from, "2026-07-16T12:15:12Z");
}

#[test]
fn period_day_range_includes_both_ends() {
    let now = Utc.with_ymd_and_hms(2026, 7, 16, 13, 45, 12).unwrap();
    let range = resolve("2026-01-01..2026-01-31", now).unwrap();

    assert_eq!(range.from, "2026-01-01T00:00:00Z");
    assert_eq!(range.to, "2026-02-01T00:00:00Z");
}

#[test]
fn period_rejects_unknown_presets_and_zero_windows() {
    let now = Utc.with_ymd_and_hms(2026, 7, 16, 13, 45, 12).unwrap();

    assert_eq!(
        resolve("nonsense", now),
        Err(PeriodError::Unknown("nonsense".to_string()))
    );
    assert_eq!(
        resolve("last:0h", now),
        Err(PeriodError::InvalidRange("last:0h".to_string()))
    );
    // A bad number, a bad unit, and a backwards day range are errors too.
    assert!(resolve("last:soon", now).is_err());
    assert!(resolve("last:5w", now).is_err());
    assert!(resolve("2026-01-31..2026-01-01", now).is_err());
}

#[test]
fn ledger_csv_writes_a_header_and_quotes_commas_and_quotes() {
    let mut first = ledger_row("req-csv-1");
    first.client_model = "gpt, \"turbo\"".to_string();
    first.agent = Some("agent,one".to_string());
    first.cost_usd = Some(0.123_456_789);
    let second = ledger_row("req-csv-2");

    let csv = ledger_csv(&[first, second]);
    let mut lines = csv.lines();
    assert_eq!(
        lines.next(),
        Some(
            "timestamp,provider,client_model,upstream_model,served_model,agent,account,\
             session_id,status,streaming,latency_ms,ttft_ms,input_tokens,output_tokens,\
             cache_read_tokens,cache_write_tokens,reasoning_tokens,cost_usd"
        )
    );

    let first_line = lines.next().unwrap();
    assert!(first_line.contains("\"gpt, \"\"turbo\"\"\""));
    assert!(first_line.contains("\"agent,one\""));
    assert!(first_line.ends_with(",0.123457"));

    let second_line = lines.next().unwrap();
    assert!(second_line.contains(",,"));
    assert_eq!(lines.next(), None);
    assert_eq!(csv.lines().count(), 3, "header plus one line per row");
}

fn ledger_row(request_id: &str) -> LedgerRow {
    LedgerRow {
        request_id: request_id.to_string(),
        timestamp: "2026-07-16T12:00:00Z".to_string(),
        provider: "kimi".to_string(),
        client_model: "gpt-5.4".to_string(),
        upstream_model: "moonshot-v1".to_string(),
        served_model: None,
        agent: None,
        account: None,
        session_id: None,
        status: 200,
        attempt: 0,
        streaming: true,
        latency_ms: 1234,
        ttft_ms: 100,
        input_tokens: 100,
        output_tokens: 10,
        cache_read_tokens: 5,
        cache_write_tokens: 2,
        reasoning_tokens: 1,
        cost_usd: Some(0.25),
    }
}
