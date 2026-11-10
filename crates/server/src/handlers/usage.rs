//! `GET /v1/usage` — what the proxy has spent, over the gateway itself.
//!
//! The Web UI reads the same numbers from the same database; this endpoint is
//! for everything that cannot reach the file: a script, a status widget, a
//! terminal on another machine.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use log::error;

use super::{AppState, api_error_response, data_error_response};
use crate::period::{self, Range};
use crate::storage::{
    Granularity, Heatmap, LedgerFilter, LedgerPage, SplitDim, SqliteStorage, StorageError, Totals,
    UsageQuery, UsageSummary,
};

/// The usage summary for a period, as JSON.
///
/// Query: `preset` (default `7d`), `granularity` (default `day`, one of
/// `hour|day|week|month`), and optional `provider`, `model`, `agent`,
/// `account`, `split_by` (one of `provider|agent|model|account`).
///
/// Requires the management key at the router boundary: this exposes spend,
/// not just liveness.
pub async fn handle_usage_api(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let preset = params.get("preset").map(String::as_str).unwrap_or("7d");
    let range = match period::resolve(preset, chrono::Utc::now()) {
        Ok(range) => range,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": err.to_string() })),
            )
                .into_response();
        }
    };

    let Some(dir) = state.config.logging.dir.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "logging is not configured" })),
        )
            .into_response();
    };

    let query = UsageQuery {
        from: range.from,
        to: range.to,
        granularity: granularity_from(params.get("granularity").map(String::as_str)),
        provider: params.get("provider").cloned(),
        model: params.get("model").cloned(),
        agent: params.get("agent").cloned(),
        account: params.get("account").cloned(),
        split_by: split_dim_from(params.get("split_by").map(String::as_str)),
    };

    match usage_summary_from_log_dir(dir, &query) {
        Ok(summary) => Json(summary).into_response(),
        Err(err) => {
            error!("usage summary failed dir={dir}: {err}");
            api_error_response(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", err)
        }
    }
}

/// `GET /v1/admin/usage` — the UI's usage data surface: summary, heatmap,
/// ledger page, or ledger CSV behind one path.
///
/// Query parameters: `preset`, `from`, `to`, `granularity`, `provider`,
/// `model`, `agent`, `account`, `split_by`, `day`, `failed`, `retried`,
/// `query`, `cursor`, `limit`, `ledger`, `heatmap`, `format`. Branch order:
/// `heatmap=1` wins over the ledger, `format=csv` implies the ledger, and
/// everything else is the summary.
pub async fn handle_admin_usage(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let range = match resolve_range(&params) {
        Ok(range) => range,
        Err(message) => {
            return data_error_response(StatusCode::BAD_REQUEST, "invalid_range", message);
        }
    };
    let granularity = granularity_from(params.get("granularity").map(String::as_str));
    let dir = state.config.logging.dir.clone();

    // The calendar has its own 53-week span, so the resolved range only has to
    // be a valid one for the request to reach this branch.
    if params.get("heatmap").map(String::as_str) == Some("1") {
        let filter = ledger_filter(&params, &range);
        let query = UsageQuery {
            from: range.from,
            to: range.to,
            granularity: Granularity::Day,
            provider: filter.provider,
            model: filter.model,
            agent: filter.agent,
            account: filter.account,
            split_by: None,
        };
        let heatmap = match dir.as_deref() {
            Some(dir) => match heatmap_from_log_dir(dir, &query) {
                Ok(heatmap) => heatmap,
                Err(err) => {
                    error!("usage heatmap failed dir={dir}: {err}");
                    return data_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "storage_error",
                        err,
                    );
                }
            },
            None => None,
        };
        return Json(serde_json::json!({ "heatmap": heatmap })).into_response();
    }

    if params.get("ledger").map(String::as_str) == Some("1")
        || params.get("format").map(String::as_str) == Some("csv")
    {
        let cursor = match parse_cursor(params.get("cursor").map(String::as_str)) {
            Ok(cursor) => cursor,
            Err(err) => return err.response(),
        };
        let limit = match parse_ledger_limit(params.get("limit").map(String::as_str)) {
            Ok(limit) => limit,
            Err(err) => return err.response(),
        };
        let filter = ledger_filter(&params, &range);
        let page = match dir.as_deref() {
            Some(dir) => match ledger_page_from_log_dir(dir, &filter, cursor, limit) {
                Ok(page) => page,
                Err(err) => {
                    error!("usage ledger failed dir={dir}: {err}");
                    return data_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "storage_error",
                        err,
                    );
                }
            },
            None => empty_ledger_page(),
        };
        if params.get("format").map(String::as_str) == Some("csv") {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/csv; charset=utf-8")],
                crate::ledger_csv(&page.rows),
            )
                .into_response();
        }
        return Json(serde_json::json!({ "page": page })).into_response();
    }

    let query = UsageQuery {
        from: range.from,
        to: range.to,
        granularity,
        provider: params.get("provider").cloned(),
        model: params.get("model").cloned(),
        agent: params.get("agent").cloned(),
        account: params.get("account").cloned(),
        split_by: split_dim_from(params.get("split_by").map(String::as_str)),
    };
    let summary = match dir.as_deref() {
        Some(dir) => match usage_summary_from_log_dir(dir, &query) {
            Ok(summary) => summary,
            Err(err) => {
                error!("usage summary failed dir={dir}: {err}");
                return data_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    err,
                );
            }
        },
        None => empty_usage_summary(&query),
    };
    Json(serde_json::json!({ "summary": summary })).into_response()
}

/// The range the request asks for: an explicit `from`/`to` pair (both RFC
/// 3339, both present — the hook layer works in concrete ranges) wins over
/// the `preset`, defaulting to `7d` like the Next route.
fn resolve_range(params: &HashMap<String, String>) -> Result<Range, String> {
    match (params.get("from"), params.get("to")) {
        (Some(from), Some(to)) => {
            for (name, value) in [("from", from), ("to", to)] {
                chrono::DateTime::parse_from_rfc3339(value)
                    .map_err(|_| format!("invalid {name}: expected an RFC 3339 timestamp"))?;
            }
            Ok(Range {
                from: from.clone(),
                to: to.clone(),
            })
        }
        _ => period::resolve(
            params.get("preset").map(String::as_str).unwrap_or("7d"),
            chrono::Utc::now(),
        )
        .map_err(|err| err.to_string()),
    }
}

/// The ledger filters the heatmap and ledger branches share.
fn ledger_filter(params: &HashMap<String, String>, range: &Range) -> LedgerFilter {
    LedgerFilter {
        from: range.from.clone(),
        to: range.to.clone(),
        day: params.get("day").cloned(),
        provider: params.get("provider").cloned(),
        model: params.get("model").cloned(),
        agent: params.get("agent").cloned(),
        account: params.get("account").cloned(),
        failed: flag(params, "failed"),
        retried: flag(params, "retried"),
        query: params.get("query").cloned(),
    }
}

/// `1` or `true` set a boolean filter flag.
fn flag(params: &HashMap<String, String>, key: &str) -> bool {
    matches!(params.get(key).map(String::as_str), Some("1" | "true"))
}

/// Why a query parameter failed to parse; the handler turns it into the
/// data error envelope.
#[derive(Debug, Clone, Copy)]
enum ParamError {
    Cursor,
    Limit,
}

impl ParamError {
    fn response(self) -> Response {
        match self {
            Self::Cursor => data_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "cursor must be a non-negative integer",
            ),
            Self::Limit => data_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_limit",
                "limit must be a non-negative integer",
            ),
        }
    }
}

fn parse_cursor(value: Option<&str>) -> Result<Option<i64>, ParamError> {
    match value {
        None => Ok(None),
        Some(raw) => match raw.trim().parse::<i64>() {
            Ok(parsed) if parsed >= 0 => Ok(Some(parsed)),
            _ => Err(ParamError::Cursor),
        },
    }
}

/// `limit`: absent means the storage default (0), anything that is not a
/// non-negative integer is a 400.
fn parse_ledger_limit(value: Option<&str>) -> Result<u64, ParamError> {
    match value {
        None => Ok(0),
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(parsed) => Ok(parsed),
            Err(_) => Err(ParamError::Limit),
        },
    }
}

/// Reads the heatmap from the log database, treating "no database yet" as no
/// calendar: the UI renders an empty grid instead of a broken one.
fn heatmap_from_log_dir(dir: &str, q: &UsageQuery) -> Result<Option<Heatmap>, StorageError> {
    if !std::path::Path::new(dir)
        .join("any-converter.sqlite3")
        .exists()
    {
        return Ok(None);
    }
    match SqliteStorage::open_readonly_in_log_dir(dir).and_then(|s| s.heatmap(q)) {
        Ok(heatmap) => Ok(Some(heatmap)),
        Err(err) if err.is_missing_table() => Ok(None),
        Err(err) => Err(err),
    }
}

/// Reads one ledger page from the log database; a missing database or table
/// is an empty page, the same "nothing served yet" answer the summary gives.
fn ledger_page_from_log_dir(
    dir: &str,
    filter: &LedgerFilter,
    cursor: Option<i64>,
    limit: u64,
) -> Result<LedgerPage, StorageError> {
    if !std::path::Path::new(dir)
        .join("any-converter.sqlite3")
        .exists()
    {
        return Ok(empty_ledger_page());
    }
    match SqliteStorage::open_readonly_in_log_dir(dir)
        .and_then(|s| s.ledger_page(filter, cursor, limit))
    {
        Ok(page) => Ok(page),
        Err(err) if err.is_missing_table() => Ok(empty_ledger_page()),
        Err(err) => Err(err),
    }
}

fn empty_ledger_page() -> LedgerPage {
    LedgerPage {
        rows: Vec::new(),
        total: 0,
        totals: Totals::default(),
        next_cursor: None,
    }
}

/// Reads the summary from the log database, treating "no database yet" as an
/// empty one: a gateway that has served nothing should answer "no spend", not
/// fail. A database without the rollup table reads the same way; any other
/// storage failure is still reported.
fn usage_summary_from_log_dir(dir: &str, q: &UsageQuery) -> Result<UsageSummary, StorageError> {
    if !std::path::Path::new(dir)
        .join("any-converter.sqlite3")
        .exists()
    {
        return Ok(empty_usage_summary(q));
    }
    match SqliteStorage::open_readonly_in_log_dir(dir).and_then(|s| s.usage_summary(q)) {
        Ok(summary) => Ok(summary),
        Err(err) if err.is_missing_table() => Ok(empty_usage_summary(q)),
        Err(err) => Err(err),
    }
}

fn empty_usage_summary(q: &UsageQuery) -> UsageSummary {
    UsageSummary {
        from: q.from.clone(),
        to: q.to.clone(),
        granularity: q.granularity,
        totals: Totals::default(),
        series: Vec::new(),
        by_provider: Vec::new(),
        by_model: Vec::new(),
        by_agent: Vec::new(),
        by_account: Vec::new(),
        by_session: Vec::new(),
    }
}

fn granularity_from(value: Option<&str>) -> Granularity {
    match value {
        Some("hour") => Granularity::Hour,
        Some("week") => Granularity::Week,
        Some("month") => Granularity::Month,
        _ => Granularity::Day,
    }
}

/// `provider|agent|model|account`; anything else asks for no split.
fn split_dim_from(value: Option<&str>) -> Option<SplitDim> {
    match value {
        Some("provider") => Some(SplitDim::Provider),
        Some("agent") => Some(SplitDim::Agent),
        Some("model") => Some(SplitDim::Model),
        Some("account") => Some(SplitDim::Account),
        _ => None,
    }
}
