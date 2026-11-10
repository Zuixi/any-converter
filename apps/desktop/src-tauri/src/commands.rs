use serde::Deserialize;
use tauri::State;

use crate::server_manager::ServerStatus;
use crate::server_proxy::{self, ProxyError};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ServerRequestArgs {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub if_match: Option<String>,
}

/// Thin IPC entrypoint forwarding one management call to the embedded
/// server; the webview must never `fetch` the server cross-origin. The
/// typed `ProxyError` rejects as a structured object, not a JSON string.
#[tauri::command]
pub async fn server_request(
    state: State<'_, AppState>,
    args: ServerRequestArgs,
) -> Result<serde_json::Value, ProxyError> {
    server_proxy::forward(
        state.server_manager(),
        &state.home(),
        &args.method,
        &args.path,
        args.body.as_deref(),
        args.if_match.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    Ok(state.server_manager().status().await)
}

#[tauri::command]
pub async fn start_server(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    state
        .server_manager()
        .start(state.home().clone())
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
        .restart(state.home().clone())
        .await
        .map_err(to_error)
}

fn to_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
