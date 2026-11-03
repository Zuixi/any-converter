//! `/v1/agents/*` REST handlers and API error mapping.
//!
//! All agent state and file mutations live in [`crate::agents_registry`]; this
//! module only translates HTTP requests into registry calls and maps
//! [`ApiError`] to the uniform JSON error envelope shared by the CLI/WebUI.

use std::sync::Arc;

use axum::{
    Json,
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use crate::agents_registry::{STATUS_CONNECTED, STATUS_DISCONNECTED, spec_for};
use crate::auth::{self, AuthError};
use crate::handlers::AppState;

pub use crate::agents_registry::{AgentRegistry, AgentStatus, KeyInfo, catalog_from_config};

/// Error codes shared by the CLI, WebUI, and REST responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The agent is not installed / has no configuration location.
    AgentAbsent(String),
    /// The server has no usable models in its catalog.
    NoProviders,
    /// A configuration file could not be written.
    ConfigUnwritable(String),
    /// Disconnect ran without the pre-connect recovery values.
    StashMissing(String),
    /// The requested model is not in the catalog.
    ModelNotFound(String),
    /// The agent id is not one of the supported specs.
    UnknownAgent(String),
    /// The agent has no active wiring to sync or rotate.
    NotConnected(String),
    /// An unexpected internal failure.
    Internal(String),
}

impl ApiError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::AgentAbsent(_) => "agent_absent",
            Self::NoProviders => "server_no_providers",
            Self::ConfigUnwritable(_) => "config_unwritable",
            Self::StashMissing(_) => "stash_missing",
            Self::ModelNotFound(_) => "model_not_found",
            Self::UnknownAgent(_) => "unknown_agent",
            Self::NotConnected(_) => "not_connected",
            Self::Internal(_) => "internal",
        }
    }

    /// HTTP status appropriate for this code.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::AgentAbsent(_) | Self::UnknownAgent(_) => StatusCode::NOT_FOUND,
            Self::NoProviders | Self::ModelNotFound(_) => StatusCode::BAD_REQUEST,
            Self::StashMissing(_) | Self::NotConnected(_) => StatusCode::CONFLICT,
            Self::ConfigUnwritable(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Human-readable message.
    pub fn message(&self) -> String {
        match self {
            Self::AgentAbsent(agent) => format!("agent {agent} is not installed"),
            Self::NoProviders => "the server has no providers configured".to_owned(),
            Self::ConfigUnwritable(detail) => format!("could not update agent config: {detail}"),
            Self::StashMissing(agent) => format!("missing recovery data for {agent}"),
            Self::ModelNotFound(model) => format!("model {model} is not in the catalog"),
            Self::UnknownAgent(agent) => format!("unknown agent: {agent}"),
            Self::NotConnected(agent) => format!("agent {agent} is not connected"),
            Self::Internal(detail) => format!("internal error: {detail}"),
        }
    }

    /// Optional action hint for the client.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::NoProviders => Some("add at least one provider to the server config"),
            Self::StashMissing(_) => {
                Some("restore the .before-any-converter backup manually if needed")
            }
            Self::NotConnected(_) => Some("connect the agent before syncing"),
            _ => None,
        }
    }

    /// Agent id associated with the error, when applicable.
    pub fn agent(&self) -> Option<&str> {
        match self {
            Self::AgentAbsent(agent)
            | Self::StashMissing(agent)
            | Self::UnknownAgent(agent)
            | Self::NotConnected(agent) => Some(agent),
            _ => None,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ApiError {}

impl From<agent_wiring::WiringError> for ApiError {
    fn from(error: agent_wiring::WiringError) -> Self {
        use agent_wiring::WiringError;
        match error {
            WiringError::AgentAbsent(agent) => Self::AgentAbsent(agent),
            WiringError::StashMissing(agent) => Self::StashMissing(agent),
            WiringError::ModelNotFound(model) => Self::ModelNotFound(model),
            WiringError::NoCatalog => Self::NoProviders,
            WiringError::Backup(error) => Self::ConfigUnwritable(error.to_string()),
            WiringError::Edit(error) => Self::ConfigUnwritable(error.to_string()),
            WiringError::Io(error) => Self::ConfigUnwritable(error.to_string()),
            WiringError::Refused(reason) => Self::ConfigUnwritable(reason),
        }
    }
}


// ── HTTP handlers ────────────────────────────────────────────────────

/// `GET /v1/agents`
pub async fn handle_agents_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    state
        .agents
        .set_catalog(catalog_from_config(&state.config));
    if let Some(response) = read_auth(&state, &headers).await {
        return response;
    }
    let home = state.agents.home();
    let agents = state.agents.list(&home).await;
    Json(json!({ "agents": agents })).into_response()
}

/// `POST /v1/agents/{id}/connect`
pub async fn handle_agent_connect(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(response) = management_auth(&state, &headers) {
        return response;
    }
    let model = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<ConnectBody>(&body) {
            Ok(ConnectBody { model, codex_home }) => {
                let _ = codex_home;
                model
            }
            Err(error) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    format!("invalid JSON body: {error}"),
                    None,
                    Some(&id),
                );
            }
        }
    };
    state
        .agents
        .set_catalog(catalog_from_config(&state.config));
    let home = state.agents.home();
    match state.agents.connect(&id, model, &home).await {
        Ok(report) => Json(json!({
            "agent": id,
            "status": STATUS_CONNECTED,
            "files_written": report
                .files_written
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => api_error(error),
    }
}

/// `POST /v1/agents/{id}/disconnect`
pub async fn handle_agent_disconnect(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = management_auth(&state, &headers) {
        return response;
    }
    let home = state.agents.home();
    match state.agents.disconnect(&id, &home).await {
        Ok(report) => Json(json!({
            "agent": id,
            "status": STATUS_DISCONNECTED,
            "files_written": report
                .files_written
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
            "kept_backup": report
                .kept_backup
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
            "warnings": report.warnings,
        }))
        .into_response(),
        Err(error) => api_error(error),
    }
}

/// `POST /v1/agents/{id}/sync`
pub async fn handle_agent_sync(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = management_auth(&state, &headers) {
        return response;
    }
    state
        .agents
        .set_catalog(catalog_from_config(&state.config));
    let home = state.agents.home();
    match state.agents.sync(&id, &home).await {
        Ok(()) => Json(json!({
            "agent": id,
            "status": STATUS_CONNECTED,
            "catalog_digest": state.agents.digest_now(),
        }))
        .into_response(),
        Err(error) => api_error(error),
    }
}

/// `POST /v1/agents/{id}/key/rotate`
pub async fn handle_agent_rotate_key(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = management_auth(&state, &headers) {
        return response;
    }
    state
        .agents
        .set_catalog(catalog_from_config(&state.config));
    let home = state.agents.home();
    match state.agents.rotate_key(&id, &home).await {
        Ok(key_id) => Json(json!({ "agent": id, "key_id": key_id })).into_response(),
        Err(error) => api_error(error),
    }
}

/// `GET /v1/agents/{id}/keys`
pub async fn handle_agent_keys(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = read_auth(&state, &headers).await {
        return response;
    }
    if spec_for(&id).is_err() {
        return api_error(ApiError::UnknownAgent(id));
    }
    let keys: Vec<KeyInfo> = state.agents.key_info(&id).await.into_iter().collect();
    Json(json!({ "agent": id, "keys": keys })).into_response()
}

#[derive(Debug, Deserialize)]
struct ConnectBody {
    #[serde(default)]
    model: Option<String>,
    #[serde(default, rename = "codexHome")]
    codex_home: Option<String>,
}

async fn read_auth(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    match state
        .agents
        .validate_key(state.config.server.api_key.as_deref(), header)
        .await
    {
        Ok(()) => None,
        Err(error) => Some(auth_response(error)),
    }
}

fn management_auth(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    match auth::require_management_key(state.config.server.api_key.as_deref(), header) {
        Ok(()) => None,
        Err(AuthError::Missing) => Some(error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing management API key",
            None,
            None,
        )),
        Err(AuthError::Invalid) => Some(error_response(
            StatusCode::FORBIDDEN,
            "management_forbidden",
            "agent keys cannot manage agents; use the server API key",
            Some("send the global server API key, not an agent key"),
            None,
        )),
    }
}

fn auth_response(error: AuthError) -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "authentication_error",
        error.to_string(),
        None,
        None,
    )
}

fn api_error(error: ApiError) -> Response {
    error_response(
        error.status(),
        error.code(),
        error.message(),
        error.hint(),
        error.agent(),
    )
}

fn error_response(
    status: StatusCode,
    code: &str,
    message: impl std::fmt::Display,
    hint: Option<&str>,
    agent: Option<&str>,
) -> Response {
    let mut error = json!({
        "code": code,
        "message": message.to_string(),
    });
    if let Some(hint) = hint {
        error["hint"] = json!(hint);
    }
    if let Some(agent) = agent {
        error["agent"] = json!(agent);
    }
    (status, Json(json!({ "error": error }))).into_response()
}
