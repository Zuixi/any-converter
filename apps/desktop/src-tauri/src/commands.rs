use serde::Deserialize;
use tauri::State;

use crate::server_manager::ServerStatus;
use crate::server_proxy::{self, ProxyError};
use crate::state::AppState;

/// IPC payload for [`server_request`].
///
/// The webview sends `ServerProxyRequest` from `apps/desktop/src/types`; its
/// fields are camelCase (`ifMatch`). Accept that casing here explicitly:
/// serde ignores unknown fields by default, so a casing mismatch silently
/// drops the value instead of failing — which is how `If-Match` once
/// disappeared between the webview and the gateway. `deny_unknown_fields`
/// turns any future mismatch into a loud bad-request rejection.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn server_request_args_deserialize_the_camel_case_ipc_payload() {
        // Mirrors `ServerProxyRequest` as `invoke("server_request", ...)`
        // serializes it; dropping `ifMatch` here silently removes the
        // ETag precondition from every management mutation.
        let args: ServerRequestArgs = serde_json::from_str(
            r#"{"method":"PATCH","path":"/v1/admin/config","body":"{}","ifMatch":"\"abc\""}"#,
        )
        .unwrap();
        assert_eq!(args.method, "PATCH");
        assert_eq!(args.if_match.as_deref(), Some("\"abc\""));
    }

    #[test]
    fn server_request_args_omit_optional_fields_cleanly() {
        let args: ServerRequestArgs =
            serde_json::from_str(r#"{"method":"GET","path":"/v1/admin/config"}"#).unwrap();
        assert!(args.body.is_none());
        assert!(args.if_match.is_none());
    }

    #[test]
    fn server_request_args_reject_unknown_fields() {
        // A snake_case or misspelled key must fail loudly instead of being
        // ignored and losing a security-relevant header.
        let error = serde_json::from_str::<ServerRequestArgs>(
            r#"{"method":"PATCH","path":"/v1/admin/config","ifMatch":"x","extra":1}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
