use std::collections::HashMap;
use std::sync::Arc;

use any_converter_core::convert::{convert_request, convert_response, Format};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use reqwest::Client;
use serde_json::Value;
use tracing::{debug, error, info};

use crate::auth::{self, AuthError};
use crate::config::ServerConfig;
use crate::proxy::build_upstream_url;
use crate::router::RouteInfo;

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub client: Client,
}

pub async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

pub async fn handle_models(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let mut seen = std::collections::HashSet::new();
    let is_codex = params.contains_key("client_version");

    if is_codex {
        let mut models = Vec::new();
        for provider in &state.config.providers {
            for model in provider.model_map.keys() {
                if model == "*" || !seen.insert(model.clone()) {
                    continue;
                }
                models.push(build_codex_model_object(
                    model,
                    state.config.model_metadata.get(model),
                ));
            }
        }
        return Json(serde_json::json!({ "models": models }));
    }

    let mut data = Vec::new();
    for provider in &state.config.providers {
        for model in provider.model_map.keys() {
            if model == "*" || !seen.insert(model.clone()) {
                continue;
            }
            data.push(build_model_object(
                model,
                &provider.name,
                state.config.model_metadata.get(model),
            ));
        }
    }

    Json(serde_json::json!({
        "object": "list",
        "data": data,
    }))
}

pub async fn handle_model_retrieve(
    State(state): State<Arc<AppState>>,
    Path(model_id): Path<String>,
) -> Response {
    for provider in &state.config.providers {
        if provider.model_map.contains_key(&model_id) {
            return Json(build_model_object(
                &model_id,
                &provider.name,
                state.config.model_metadata.get(&model_id),
            ))
            .into_response();
        }
    }

    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": {
                "message": format!("The model '{}' does not exist", model_id),
                "type": "invalid_request_error",
                "code": "model_not_found",
            }
        })),
    )
        .into_response()
}

fn build_model_object(
    model_id: &str,
    owner: &str,
    metadata: Option<&crate::config::ModelMetadata>,
) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "id": model_id,
        "slug": model_id,
        "object": "model",
        "created": 1700000000u64,
        "owned_by": owner,
    });

    if let Some(meta) = metadata {
        if let Some(cw) = meta.context_window {
            obj["context_window"] = serde_json::json!(cw);
        }
        if let Some(mcw) = meta.max_context_window {
            obj["max_context_window"] = serde_json::json!(mcw);
        }
        if let Some(sptc) = meta.supports_parallel_tool_calls {
            obj["supports_parallel_tool_calls"] = serde_json::json!(sptc);
        }
    }

    obj
}

/// Build a Codex CLI-compatible ModelInfo object.
/// Codex deserializes `/models` responses as `ModelsResponse { models: Vec<ModelInfo> }`
/// and many fields are required (no `#[serde(default)]`).
fn build_codex_model_object(
    model_id: &str,
    metadata: Option<&crate::config::ModelMetadata>,
) -> serde_json::Value {
    let ctx = metadata.and_then(|m| m.context_window).unwrap_or(272_000);
    let max_ctx = metadata
        .and_then(|m| m.max_context_window)
        .unwrap_or(ctx);
    let par_tools = metadata
        .and_then(|m| m.supports_parallel_tool_calls)
        .unwrap_or(false);
    let display = metadata
        .and_then(|m| m.display_name.as_deref())
        .unwrap_or(model_id);
    let desc = metadata
        .and_then(|m| m.description.as_deref())
        .unwrap_or("");

    serde_json::json!({
        "slug": model_id,
        "display_name": display,
        "description": desc,
        "default_reasoning_level": null,
        "supported_reasoning_levels": [],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "base_instructions": "",
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "truncation_policy": { "mode": "bytes", "limit": ctx },
        "supports_parallel_tool_calls": par_tools,
        "context_window": ctx,
        "max_context_window": max_ctx,
        "experimental_supported_tools": [],
    })
}

pub async fn handle_openai_chat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    process_request(
        &state,
        &headers,
        &body,
        RouteInfo {
            client_format: Format::OpenAIChat,
            path_streaming: false,
        },
        None,
    )
    .await
}

pub async fn handle_claude(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    process_request(
        &state,
        &headers,
        &body,
        RouteInfo {
            client_format: Format::Claude,
            path_streaming: false,
        },
        None,
    )
    .await
}

pub async fn handle_responses(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    process_request(
        &state,
        &headers,
        &body,
        RouteInfo {
            client_format: Format::OpenAIResponses,
            path_streaming: false,
        },
        None,
    )
    .await
}

pub async fn handle_gemini(
    State(state): State<Arc<AppState>>,
    Path(model_action): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some((model, path_streaming)) = parse_gemini_model_action(&model_action) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "message": "invalid gemini model path",
                    "type": "invalid_request",
                }
            })),
        )
            .into_response();
    };

    process_request(
        &state,
        &headers,
        &body,
        RouteInfo {
            client_format: Format::Gemini,
            path_streaming,
        },
        Some(&model),
    )
    .await
}

fn parse_gemini_model_action(model_action: &str) -> Option<(String, bool)> {
    if let Some(model) = model_action.strip_suffix(":streamGenerateContent") {
        Some((model.to_string(), true))
    } else if let Some(model) = model_action.strip_suffix(":generateContent") {
        Some((model.to_string(), false))
    } else {
        None
    }
}

async fn process_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    route_info: RouteInfo,
    gemini_model: Option<&str>,
) -> Response {
    let req_id = uuid::Uuid::new_v4();

    info!(
        client_format = %route_info.client_format,
        body_len = body.len(),
        request_id = %req_id,
        "incoming request"
    );

    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    if let Err(err) = auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
    {
        return auth_error_response(err);
    }

    let route = match state.config.find_route(route_info.client_format) {
        Some(r) => r,
        None => {
            error!(
                request_id = %req_id,
                "no route configured for format {}",
                route_info.client_format
            );
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("no route configured for format {}", route_info.client_format),
                        "type": "route_not_found",
                    }
                })),
            )
                .into_response();
        }
    };

    let provider = match state.config.find_provider(&route.provider) {
        Some(p) => p,
        None => {
            error!(request_id = %req_id, "provider not found: {}", route.provider);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("provider not found: {}", route.provider),
                        "type": "provider_not_found",
                    }
                })),
            )
                .into_response();
        }
    };

    let client_model = extract_model_from_body(body)
        .unwrap_or_else(|| gemini_model.unwrap_or("default").to_string());
    let upstream_model = provider.resolve_model(&client_model);

    // Conversion logging: original request
    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            request_id = %req_id,
            client_format = %route_info.client_format,
            provider_format = %provider.format,
            phase = "request_original",
            body = %truncated_body(body, 4096),
        );
    }

    let ns_map = extract_namespace_map(body, route_info.client_format);

    let converted_body = match safe_convert_request(body, route_info.client_format, provider.format) {
        Ok(mut bytes) => {
            // Conversion logging: converted request
            if state.config.logging.conversion_log {
                debug!(
                    target: "conversion",
                    request_id = %req_id,
                    phase = "request_converted",
                    body = %truncated_body(&bytes, 4096),
                );
            }

            if let Err(err) = patch_model_in_body(&mut bytes, provider.format, &upstream_model) {
                error!(request_id = %req_id, "failed to patch model: {err}");
            }
            bytes
        }
        Err(err) => {
            error!(
                request_id = %req_id,
                from = %route_info.client_format,
                to = %provider.format,
                error = %err,
                "request conversion failed"
            );
            return conversion_error_response(err);
        }
    };

    let streaming = is_streaming_request(body, route_info);

    let url = build_upstream_url(
        provider.format,
        &provider.base_url,
        &upstream_model,
        streaming,
    );

    info!(
        request_id = %req_id,
        client_format = %route_info.client_format,
        provider = %provider.name,
        upstream_model = %upstream_model,
        streaming = streaming,
        "forwarding request"
    );

    let auth_headers = auth::build_upstream_auth_headers(provider.format, &provider.api_key);

    if streaming {
        return match crate::proxy::forward_streaming(
            &state.client,
            &url,
            converted_body,
            &auth_headers,
            provider.format,
            route_info.client_format,
            &ns_map,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => upstream_error_response(&err),
        };
    }

    match crate::proxy::forward_non_streaming(
        &state.client,
        &url,
        converted_body,
        &auth_headers,
    )
    .await
    {
        Ok((status, upstream_body)) => {
            // Conversion logging: upstream response
            if state.config.logging.conversion_log {
                debug!(
                    target: "conversion",
                    request_id = %req_id,
                    phase = "response_original",
                    status = %status,
                    body = %truncated_body(&upstream_body, 4096),
                );
            }

            let mut converted = match safe_convert_response(
                &upstream_body,
                provider.format,
                route_info.client_format,
            ) {
                Ok(bytes) => bytes,
                Err(err) => return conversion_error_response(err),
            };

            patch_response_namespaces(&mut converted, &ns_map);

            // Conversion logging: converted response
            if state.config.logging.conversion_log {
                debug!(
                    target: "conversion",
                    request_id = %req_id,
                    phase = "response_converted",
                    body = %truncated_body(&converted, 4096),
                );
            }

            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(converted))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(err) => upstream_error_response(&err),
    }
}

fn truncated_body(body: &[u8], max_len: usize) -> String {
    let s = String::from_utf8_lossy(body);
    if s.len() <= max_len {
        s.into_owned()
    } else {
        format!("{}...<truncated {} bytes>", &s[..max_len], s.len() - max_len)
    }
}

fn auth_error_response(err: AuthError) -> Response {
    let status = match err {
        AuthError::Missing | AuthError::Invalid => StatusCode::UNAUTHORIZED,
    };
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": err.to_string(),
                "type": "authentication_error",
            }
        })),
    )
        .into_response()
}

fn upstream_error_response(err: &str) -> Response {
    error!("upstream error: {err}");
    (
        StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({
            "error": {
                "message": err,
                "type": "upstream_error",
            }
        })),
    )
        .into_response()
}

fn conversion_error_response(err: HandlerError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": {
                "message": err.to_string(),
                "type": "conversion_error",
            }
        })),
    )
        .into_response()
}

#[derive(Debug)]
enum HandlerError {
    Conversion(any_converter_core::error::ConvertError),
    Panic,
}

impl std::fmt::Display for HandlerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandlerError::Conversion(e) => write!(f, "{e}"),
            HandlerError::Panic => write!(f, "conversion not implemented"),
        }
    }
}

fn safe_convert_request(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, HandlerError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        convert_request(input, from, to)
    })) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => Err(HandlerError::Conversion(e)),
        Err(_) => Err(HandlerError::Panic),
    }
}

fn safe_convert_response(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, HandlerError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        convert_response(input, from, to)
    })) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => Err(HandlerError::Conversion(e)),
        Err(_) => Err(HandlerError::Panic),
    }
}

/// Detect whether the request should be streamed.
pub fn is_streaming_request(body: &[u8], route_info: RouteInfo) -> bool {
    if route_info.path_streaming {
        return true;
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false)
}

/// Extract namespace mapping from Responses API request tools.
/// Returns a map: qualified_name → (namespace, short_name).
/// For `{type:"namespace", name:"mcp__srv", tools:[{name:"fn1"}]}`,
/// the entry is `"mcp__srv__fn1" → ("mcp__srv", "fn1")`.
pub(crate) fn extract_namespace_map(
    body: &[u8],
    client_format: Format,
) -> HashMap<String, (String, String)> {
    if client_format != Format::OpenAIResponses {
        return HashMap::new();
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return HashMap::new();
    };
    let Some(tools) = value.get("tools").and_then(|v| v.as_array()) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for tool in tools {
        if tool.get("type").and_then(|v| v.as_str()) != Some("namespace") {
            continue;
        }
        let ns = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if ns.is_empty() {
            continue;
        }
        let Some(children) = tool.get("tools").and_then(|v| v.as_array()) else {
            continue;
        };
        for child in children {
            if let Some(name) = child.get("name").and_then(|v| v.as_str()) {
                let qualified = format!("{ns}__{name}");
                map.insert(qualified, (ns.to_string(), name.to_string()));
            }
        }
    }
    map
}

/// Patch function_call items in a Responses API response body to include
/// the `namespace` field and restore the short tool name. This is required
/// for Codex CLI which dispatches tool calls via `ToolName{namespace, name}`.
fn patch_response_namespaces(body: &mut Vec<u8>, ns_map: &HashMap<String, (String, String)>) {
    if ns_map.is_empty() {
        return;
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return;
    };
    let mut patched = false;
    if let Some(output) = value.get_mut("output").and_then(|v| v.as_array_mut()) {
        for item in output {
            if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
                continue;
            }
            if let Some(name) = item.get("name").and_then(|v| v.as_str()).map(String::from) {
                if let Some((ns, short)) = ns_map.get(&name) {
                    item["name"] = Value::String(short.clone());
                    item["namespace"] = Value::String(ns.clone());
                    patched = true;
                }
            }
        }
    }
    if patched {
        if let Ok(bytes) = serde_json::to_vec(&value) {
            *body = bytes;
        }
    }
}

fn extract_model_from_body(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(str::to_string))
}

fn patch_model_in_body(
    body: &mut Vec<u8>,
    format: Format,
    model: &str,
) -> Result<(), serde_json::Error> {
    let mut value: Value = serde_json::from_slice(body)?;
    match format {
        Format::Gemini => {
            if value.get("model").is_some() {
                value["model"] = Value::String(model.to_string());
            }
        }
        _ => {
            value["model"] = Value::String(model.to_string());
        }
    }
    *body = serde_json::to_vec(&value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::config::{LoggingConfig, ProviderConfig, RouteConfig, ServerSettings};

    fn sample_state() -> AppState {
        AppState {
            config: ServerConfig {
                server: ServerSettings {
                    host: "127.0.0.1".into(),
                    port: 8080,
                    api_key: None,
                },
                providers: vec![ProviderConfig {
                    name: "openai".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://api.openai.com".into(),
                    api_key: "sk-test".into(),
                    model_map: [("*".into(), "gpt-4.1".into())].into(),
                }],
                routes: vec![RouteConfig {
                    client_format: Format::Claude,
                    provider: "openai".into(),
                }],
                model_metadata: HashMap::new(),
                logging: LoggingConfig::default(),
            },
            client: Client::new(),
        }
    }

    #[test]
    fn test_is_streaming_request_from_body() {
        let body = br#"{"model":"claude-3","stream":true}"#;
        let route = RouteInfo {
            client_format: Format::Claude,
            path_streaming: false,
        };
        assert!(is_streaming_request(body, route));
    }

    #[test]
    fn test_is_streaming_request_from_path() {
        let body = br#"{"model":"gemini-pro"}"#;
        let route = RouteInfo {
            client_format: Format::Gemini,
            path_streaming: true,
        };
        assert!(is_streaming_request(body, route));
    }

    #[test]
    fn test_is_streaming_request_non_stream() {
        let body = br#"{"model":"claude-3","stream":false}"#;
        let route = RouteInfo {
            client_format: Format::Claude,
            path_streaming: false,
        };
        assert!(!is_streaming_request(body, route));
    }

    #[tokio::test]
    async fn test_missing_route_returns_404() {
        let state = Arc::new(AppState {
            config: ServerConfig {
                server: ServerSettings {
                    host: "127.0.0.1".into(),
                    port: 8080,
                    api_key: None,
                },
                providers: vec![],
                routes: vec![],
                model_metadata: HashMap::new(),
                logging: LoggingConfig::default(),
            },
            client: Client::new(),
        });

        let route_info = RouteInfo {
            client_format: Format::Claude,
            path_streaming: false,
        };
        let response = process_request(
            &state,
            &HeaderMap::new(),
            br#"{"model":"claude-3","messages":[]}"#,
            route_info,
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_non_streaming_flow_helpers() {
        let state = sample_state();
        let body = br#"{"model":"claude-3","stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
        let route = RouteInfo {
            client_format: Format::Claude,
            path_streaming: false,
        };

        assert!(!is_streaming_request(body, route));
        assert_eq!(
            state.config.find_route(route.client_format).unwrap().provider,
            "openai"
        );
        assert_eq!(
            state
                .config
                .find_provider("openai")
                .unwrap()
                .resolve_model("claude-3"),
            "gpt-4.1"
        );
    }
}
