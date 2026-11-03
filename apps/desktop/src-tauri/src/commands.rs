use any_converter_core::convert::{Format, convert_request, convert_response};
use any_converter_server::request_log::RequestLogRecord;
use any_converter_server::storage::{
    Granularity, Heatmap, LedgerFilter, LedgerPage, SqliteStorage, Totals, UsageQuery, UsageSummary,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::State;

use crate::db::{DesktopModelRoute, DesktopProvider};
use crate::secrets;
use crate::server_manager::ServerStatus;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct UpdateSettingRequest {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateProviderRequest {
    pub name: String,
    pub format: String,
    pub base_url: String,
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateModelRouteRequest {
    pub pattern: String,
    pub provider_ids: Vec<i64>,
    pub upstream_model: Option<String>,
    pub strategy: String,
}

#[derive(Debug, Deserialize)]
pub struct ConvertPayloadRequest {
    pub input: String,
    pub from: String,
    pub to: String,
    pub mode: String,
}

#[derive(Debug, Serialize)]
pub struct ConvertPayloadResponse {
    pub output: String,
}

#[tauri::command]
pub async fn get_settings(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, String>, String> {
    state.db().settings().map_err(to_error)
}

#[tauri::command]
pub async fn update_setting(
    state: State<'_, AppState>,
    request: UpdateSettingRequest,
) -> Result<std::collections::HashMap<String, String>, String> {
    let db = state.db();
    db.upsert_setting(&request.key, &request.value)
        .map_err(to_error)?;
    db.settings().map_err(to_error)
}

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<DesktopProvider>, String> {
    state.db().list_providers().map_err(to_error)
}

#[tauri::command]
pub async fn create_provider(
    state: State<'_, AppState>,
    request: CreateProviderRequest,
) -> Result<Vec<DesktopProvider>, String> {
    let format = parse_format(&request.format)?;
    let keychain_ref =
        secrets::set_provider_key(&request.name, &request.api_key).map_err(to_error)?;
    let db = state.db();
    db.create_provider(&request.name, format, &request.base_url, &keychain_ref)
        .map_err(to_error)?;
    db.list_providers().map_err(to_error)
}

#[tauri::command]
pub async fn delete_provider(
    state: State<'_, AppState>,
    id: i64,
) -> Result<Vec<DesktopProvider>, String> {
    let db = state.db();
    if let Some(provider) = db
        .list_providers()
        .map_err(to_error)?
        .into_iter()
        .find(|provider| provider.id == id)
    {
        let _ = secrets::delete_provider_key(&provider.name);
    }
    db.delete_provider(id).map_err(to_error)?;
    db.list_providers().map_err(to_error)
}

#[tauri::command]
pub async fn list_model_routes(
    state: State<'_, AppState>,
) -> Result<Vec<DesktopModelRoute>, String> {
    state.db().list_model_routes().map_err(to_error)
}

#[tauri::command]
pub async fn create_model_route(
    state: State<'_, AppState>,
    request: CreateModelRouteRequest,
) -> Result<Vec<DesktopModelRoute>, String> {
    let db = state.db();
    db.create_model_route(
        &request.pattern,
        request.provider_ids,
        request.upstream_model.as_deref(),
        &request.strategy,
    )
    .map_err(to_error)?;
    db.list_model_routes().map_err(to_error)
}

#[tauri::command]
pub async fn convert_payload(
    request: ConvertPayloadRequest,
) -> Result<ConvertPayloadResponse, String> {
    let from = parse_format(&request.from)?;
    let to = parse_format(&request.to)?;
    let input = request.input.into_bytes();
    let output = match request.mode.as_str() {
        "request" => convert_request(&input, from, to),
        "response" => convert_response(&input, from, to),
        other => return Err(format!("unsupported conversion mode: {other}")),
    }
    .map_err(to_error)?;
    Ok(ConvertPayloadResponse {
        output: String::from_utf8_lossy(&output).to_string(),
    })
}

#[tauri::command]
pub async fn get_heatmap(
    state: State<'_, AppState>,
    filter: LedgerFilter,
) -> Result<Heatmap, String> {
    let path = state.log_dir().join("any-converter.sqlite3");
    if !path.exists() {
        return Err("no log database yet".to_string());
    }
    SqliteStorage::open_readonly_in_log_dir(state.log_dir())
        .map_err(to_error)?
        .heatmap(&UsageQuery {
            from: filter.from.clone(),
            to: filter.to.clone(),
            granularity: Granularity::Day,
            provider: filter.provider.clone(),
            model: filter.model.clone(),
            agent: filter.agent.clone(),
            account: filter.account.clone(),
            split_by: None,
        })
        .map_err(to_error)
}

#[tauri::command]
pub async fn list_request_logs(
    state: State<'_, AppState>,
    limit: Option<u64>,
) -> Result<Vec<RequestLogRecord>, String> {
    list_request_logs_from_log_dir(state.log_dir(), limit.unwrap_or(500))
}

#[tauri::command]
pub async fn get_usage_summary(
    state: State<'_, AppState>,
    preset: Option<String>,
    granularity: Option<String>,
) -> Result<UsageSummary, String> {
    let range = any_converter_server::period::resolve(
        preset.as_deref().unwrap_or("7d"),
        chrono::Utc::now(),
    )
    .map_err(to_error)?;
    let query = UsageQuery {
        from: range.from,
        to: range.to,
        granularity: parse_granularity(granularity.as_deref().unwrap_or("day")),
        account: None,
        split_by: None,
        provider: None,
        model: None,
        agent: None,
    };
    usage_summary_from_log_dir(state.log_dir(), &query)
}

#[tauri::command]
pub async fn list_ledger_page(
    state: State<'_, AppState>,
    filter: LedgerFilter,
    cursor: Option<i64>,
    limit: Option<u64>,
) -> Result<LedgerPage, String> {
    let path = state.log_dir().join("any-converter.sqlite3");
    if !path.exists() {
        return Ok(empty_ledger_page());
    }
    match SqliteStorage::open_readonly_in_log_dir(state.log_dir())
        .and_then(|storage| storage.ledger_page(&filter, cursor, limit.unwrap_or(100)))
    {
        Ok(page) => Ok(page),
        Err(error) if error.is_missing_table() => Ok(empty_ledger_page()),
        Err(error) => Err(to_error(error)),
    }
}

pub fn list_request_logs_from_log_dir(
    log_dir: impl AsRef<Path>,
    limit: u64,
) -> Result<Vec<RequestLogRecord>, String> {
    let path = log_dir.as_ref().join("any-converter.sqlite3");
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Readonly: avoids "database is locked" while the embedded server is writing.
    // A db created by the desktop before the server ever logged has no `request_logs`
    // table: that is "no logs yet", but any other failure is still reported.
    match SqliteStorage::open_readonly_in_log_dir(log_dir)
        .and_then(|storage| storage.recent_request_logs(limit))
    {
        Ok(records) => Ok(records),
        Err(error) if error.is_missing_table() => Ok(Vec::new()),
        Err(error) => Err(to_error(error)),
    }
}

pub fn usage_summary_from_log_dir(
    log_dir: impl AsRef<Path>,
    query: &UsageQuery,
) -> Result<UsageSummary, String> {
    let path = log_dir.as_ref().join("any-converter.sqlite3");
    if !path.exists() {
        return Ok(empty_usage_summary(query));
    }
    // Same tolerance as list_request_logs_from_log_dir: no `request_logs` table
    // means no usage yet, but a real storage failure is still returned.
    match SqliteStorage::open_readonly_in_log_dir(log_dir)
        .and_then(|storage| storage.usage_summary(query))
    {
        Ok(summary) => Ok(summary),
        Err(error) if error.is_missing_table() => Ok(empty_usage_summary(query)),
        Err(error) => Err(to_error(error)),
    }
}

fn empty_usage_summary(query: &UsageQuery) -> UsageSummary {
    UsageSummary {
        from: query.from.clone(),
        to: query.to.clone(),
        granularity: query.granularity,
        totals: Totals::default(),
        series: Vec::new(),
        by_provider: Vec::new(),
        by_model: Vec::new(),
        by_agent: Vec::new(),
        by_account: Vec::new(),
        by_session: Vec::new(),
    }
}

fn empty_ledger_page() -> LedgerPage {
    LedgerPage {
        rows: Vec::new(),
        total: 0,
        next_cursor: None,
        totals: Totals::default(),
    }
}

fn parse_granularity(value: &str) -> Granularity {
    match value {
        "hour" => Granularity::Hour,
        "week" => Granularity::Week,
        "month" => Granularity::Month,
        _ => Granularity::Day,
    }
}

#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    Ok(state.server_manager().status().await)
}

#[tauri::command]
pub async fn start_server(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    state
        .server_manager()
        .start(state.db(), state.log_dir())
        .await
        .map_err(to_error)
}

#[tauri::command]
pub async fn stop_server(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    state.server_manager().stop().await.map_err(to_error)
}

#[tauri::command]
pub async fn restart_server(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    state
        .server_manager()
        .restart(state.db(), state.log_dir())
        .await
        .map_err(to_error)
}

fn parse_format(value: &str) -> Result<Format, String> {
    match value {
        "openai_chat" => Ok(Format::OpenAIChat),
        "openai_responses" => Ok(Format::OpenAIResponses),
        "claude" => Ok(Format::Claude),
        "gemini" => Ok(Format::Gemini),
        other => Err(format!("invalid format: {other}")),
    }
}

fn to_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use any_converter_core::ir::Usage;
    use any_converter_server::request_log::ResponseBodyKind;

    fn query() -> UsageQuery {
        UsageQuery {
            from: "2026-07-16T00:00:00Z".to_string(),
            to: "2026-07-17T00:00:00Z".to_string(),
            granularity: Granularity::Day,
            provider: None,
            model: None,
            account: None,
            split_by: None,
            agent: None,
        }
    }

    fn record() -> RequestLogRecord {
        RequestLogRecord {
            request_id: "req-from-desktop-log-db".to_string(),
            timestamp: "2026-07-16T12:00:00Z".to_string(),
            client_format: "openai_chat".to_string(),
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
            provider: "openai".to_string(),
            client_model: "gpt-4.1".to_string(),
            upstream_model: "gpt-4.1".to_string(),
            streaming: false,
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            request_body: None,
            upstream_request_body: None,
            response_status: 200,
            response_body: ResponseBodyKind::Json {
                text: "{}".to_string(),
            },
            latency_ms: 42,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cache_write_1h: None,
                reasoning_tokens: None,
            },
            trace: None,
            truncated: false,
        }
    }

    #[test]
    fn fresh_log_dir_returns_a_zeroed_summary() {
        let temp = tempfile::tempdir().unwrap();
        let summary = usage_summary_from_log_dir(temp.path().join("fresh"), &query()).unwrap();

        assert_eq!(summary.totals.calls, 0);
        assert_eq!(summary.totals.input_tokens, 0);
        assert_eq!(summary.totals.output_tokens, 0);
        assert!(summary.series.is_empty());
        assert!(summary.by_provider.is_empty());
        assert!(summary.by_model.is_empty());
        assert!(summary.by_agent.is_empty());
    }

    #[test]
    fn log_dir_with_rows_returns_non_zero_calls() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let storage = SqliteStorage::open_in_log_dir(&log_dir).unwrap();
        storage.insert_request_log(&record()).unwrap();

        let summary = usage_summary_from_log_dir(&log_dir, &query()).unwrap();

        assert_eq!(summary.totals.calls, 1);
        assert_eq!(summary.totals.input_tokens, 10);
        assert_eq!(summary.totals.output_tokens, 5);
        assert_eq!(summary.series.len(), 1);
    }

    #[test]
    fn database_without_the_usage_table_returns_a_zeroed_summary() {
        let temp = tempfile::tempdir().unwrap();
        let log_dir = temp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        rusqlite::Connection::open(log_dir.join("any-converter.sqlite3")).unwrap();

        let summary = usage_summary_from_log_dir(&log_dir, &query()).unwrap();

        assert_eq!(summary.totals.calls, 0);
        assert!(summary.series.is_empty());
    }
}
