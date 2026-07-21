use any_converter_core::convert::{Format, convert_request, convert_response};
use any_converter_server::request_log::RequestLogRecord;
use any_converter_server::storage::{HourlyUsage, SqliteStorage};
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
pub async fn list_request_logs(
    state: State<'_, AppState>,
    limit: Option<u64>,
) -> Result<Vec<RequestLogRecord>, String> {
    list_request_logs_from_log_dir(state.log_dir(), limit.unwrap_or(500))
}

#[tauri::command]
pub async fn get_usage_summary(
    state: State<'_, AppState>,
    limit: Option<u64>,
) -> Result<Vec<HourlyUsage>, String> {
    get_usage_summary_from_log_dir(state.log_dir(), limit.unwrap_or(50))
}

pub fn list_request_logs_from_log_dir(
    log_dir: impl AsRef<Path>,
    limit: u64,
) -> Result<Vec<RequestLogRecord>, String> {
    SqliteStorage::open_in_log_dir(log_dir)
        .map_err(to_error)?
        .recent_request_logs(limit)
        .map_err(to_error)
}

pub fn get_usage_summary_from_log_dir(
    log_dir: impl AsRef<Path>,
    limit: u64,
) -> Result<Vec<HourlyUsage>, String> {
    SqliteStorage::open_in_log_dir(log_dir)
        .map_err(to_error)?
        .hourly_usage_from_request_logs(limit)
        .map_err(to_error)
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
