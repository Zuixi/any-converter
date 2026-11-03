//! `GET /v1/usage` — what the proxy has spent, over the gateway itself.
//!
//! The Web UI reads the same numbers from the same database; this endpoint is
//! for everything that cannot reach the file: a script, a status widget, a
//! terminal on another machine.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use log::error;

use super::{AppState, api_error_response};
use crate::auth;
use crate::period;
use crate::storage::{
    Granularity, SplitDim, SqliteStorage, StorageError, Totals, UsageQuery, UsageSummary,
};

/// The usage summary for a period, as JSON.
///
/// Query: `preset` (default `7d`), `granularity` (default `day`, one of
/// `hour|day|week|month`), and optional `provider`, `model`, `agent`,
/// `account`, `split_by` (one of `provider|agent|model|account`).
///
/// Requires the client key when `server.api_key` is configured, exactly as the
/// inference routes do: this exposes spend, not just liveness.
pub async fn handle_usage_api(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    if auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response();
    }

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
