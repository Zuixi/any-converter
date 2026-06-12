use std::collections::HashMap;
use std::sync::Arc;

use any_converter_core::convert::{Format, convert_request, convert_response};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use log::{debug, error, info, warn};
use reqwest::Client;
use serde_json::Value;

use crate::auth::{self, AuthError};
use crate::config::ServerConfig;
use crate::proxy::build_upstream_url;
use crate::router::RouteInfo;
use crate::usage::{self, UsageLogger, UsageRecord};

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub client: Client,
    pub usage_logger: Option<Arc<UsageLogger>>,
}

pub async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

pub async fn handle_models(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let available = state.config.available_models();
    let is_codex = params.contains_key("client_version");

    if is_codex {
        let models: Vec<_> = available
            .iter()
            .map(|m| build_codex_model_object(m, state.config.model_metadata.get(m.as_str())))
            .collect();
        return Json(serde_json::json!({ "models": models }));
    }

    let data: Vec<_> = available
        .iter()
        .map(|m| {
            build_model_object(
                m,
                "any-converter",
                state.config.model_metadata.get(m.as_str()),
            )
        })
        .collect();

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

    api_error_response(
        StatusCode::NOT_FOUND,
        "model_not_found",
        format!("The model '{}' does not exist", model_id),
    )
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
    let max_ctx = metadata.and_then(|m| m.max_context_window).unwrap_or(ctx);
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
        return api_error_response(
            StatusCode::NOT_FOUND,
            "invalid_request",
            "invalid gemini model path",
        );
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
    } else {
        model_action
            .strip_suffix(":generateContent")
            .map(|model| (model.to_string(), false))
    }
}

struct RequestContext {
    req_id: uuid::Uuid,
    start_time: std::time::Instant,
    client_model: String,
    upstream_model: String,
    ns_map: std::collections::HashMap<String, (String, String)>,
    streaming: bool,
    provider_names: Vec<String>,
}

fn prepare_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    route_info: RouteInfo,
    gemini_model: Option<&str>,
) -> Result<(RequestContext, Vec<u8>), Box<Response>> {
    let req_id = uuid::Uuid::new_v4();
    let start_time = std::time::Instant::now();

    info!(
        "incoming request request_id={} client_format={} body_len={}",
        req_id,
        route_info.client_format,
        body.len()
    );

    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
        .map_err(|err| Box::new(auth_error_response(err)))?;

    let client_model = extract_model_from_body(body)
        .or_else(|| gemini_model.map(String::from))
        .unwrap_or_default();

    if client_model.is_empty() {
        warn!("no model in request body request_id={}", req_id);
    }

    let body = strip_private_fields(body);

    let resolved = state
        .config
        .resolve_provider(route_info.client_format, &client_model)
        .ok_or_else(|| {
            error!(
                "no route for model={} format={} request_id={}",
                client_model, route_info.client_format, req_id
            );
            Box::new(api_error_response(
                StatusCode::NOT_FOUND,
                "route_not_found",
                format!(
                    "no route configured for model \"{}\" (format: {})",
                    client_model, route_info.client_format
                ),
            ))
        })?;

    let ns_map = extract_namespace_map(&body, route_info.client_format);
    let streaming = is_streaming_request(&body, route_info);

    Ok((
        RequestContext {
            req_id,
            start_time,
            client_model,
            upstream_model: resolved.upstream_model,
            ns_map,
            streaming,
            provider_names: resolved.provider_names,
        },
        body,
    ))
}

fn convert_and_build_upstream(
    state: &AppState,
    body: &[u8],
    route_info: RouteInfo,
    ctx: &RequestContext,
    provider: &crate::config::ProviderConfig,
) -> Result<(Vec<u8>, String, Vec<(String, String)>), Box<Response>> {
    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            "request_original request_id={} client_format={} provider_format={} body={}",
            ctx.req_id, route_info.client_format, provider.format, truncated_body(body, 4096)
        );
    }

    let converted_body = match safe_convert_request(body, route_info.client_format, provider.format)
    {
        Ok(mut bytes) => {
            if state.config.logging.conversion_log {
                debug!(
                    target: "conversion",
                    "request_converted request_id={} body={}",
                    ctx.req_id, truncated_body(&bytes, 4096)
                );
            }
            if let Err(err) = patch_model_in_body(&mut bytes, provider.format, &ctx.upstream_model)
            {
                error!("failed to patch model: {err} request_id={}", ctx.req_id);
            }
            bytes
        }
        Err(err) => {
            error!(
                "request conversion failed request_id={} from={} to={} error={}",
                ctx.req_id, route_info.client_format, provider.format, err
            );
            return Err(Box::new(conversion_error_response(err)));
        }
    };

    let url = build_upstream_url(
        provider.format,
        &provider.base_url,
        &ctx.upstream_model,
        ctx.streaming,
    );

    let auth_headers = auth::build_upstream_auth_headers(provider.format, &provider.api_key);

    Ok((converted_body, url, auth_headers))
}

fn build_non_streaming_response(
    state: &AppState,
    upstream_body: &[u8],
    status: reqwest::StatusCode,
    route_info: RouteInfo,
    ctx: &RequestContext,
    provider: &crate::config::ProviderConfig,
) -> Result<Response, HandlerError> {
    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            "response_original request_id={} status={} body={}",
            ctx.req_id, status, truncated_body(upstream_body, 4096)
        );
    }

    let mut converted =
        safe_convert_response(upstream_body, provider.format, route_info.client_format)?;

    patch_response_namespaces(&mut converted, &ctx.ns_map);

    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            "response_converted request_id={} body={}",
            ctx.req_id, truncated_body(&converted, 4096)
        );
    }

    if let Some(ref logger) = state.usage_logger {
        let (input_tokens, output_tokens) = usage::extract_usage_from_response(upstream_body);
        logger.log(UsageRecord {
            request_id: ctx.req_id.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            client_format: route_info.client_format.to_string(),
            provider: provider.name.clone(),
            client_model: ctx.client_model.clone(),
            upstream_model: ctx.upstream_model.clone(),
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            latency_ms: ctx.start_time.elapsed().as_millis() as u64,
            status: status.as_u16(),
            streaming: false,
        });
    }

    Ok(Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(converted))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

async fn process_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    route_info: RouteInfo,
    gemini_model: Option<&str>,
) -> Response {
    let (ctx, body) = match prepare_request(state, headers, body, route_info, gemini_model) {
        Ok(pair) => pair,
        Err(response) => return *response,
    };

    let mut last_error: Option<String> = None;

    for (attempt, provider_name) in ctx.provider_names.iter().enumerate() {
        let provider = match state.config.find_provider(provider_name) {
            Some(p) => p,
            None => {
                error!(
                    "provider not found: {} request_id={}",
                    provider_name, ctx.req_id
                );
                last_error = Some(format!("provider not found: {provider_name}"));
                continue;
            }
        };

        let (converted_body, url, auth_headers) =
            match convert_and_build_upstream(state, &body, route_info, &ctx, provider) {
                Ok(v) => v,
                Err(response) => return *response,
            };

        info!(
            "forwarding request request_id={} client_format={} provider={} upstream_model={} streaming={} attempt={}",
            ctx.req_id,
            route_info.client_format,
            provider.name,
            ctx.upstream_model,
            ctx.streaming,
            attempt + 1
        );

        if ctx.streaming {
            return match crate::proxy::forward_streaming(
                &state.client,
                &url,
                converted_body,
                &auth_headers,
                provider.format,
                route_info.client_format,
                &ctx.ns_map,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => {
                    warn!(
                        "streaming forward failed request_id={} provider={} error={}",
                        ctx.req_id, provider.name, err
                    );
                    last_error = Some(err);
                    continue;
                }
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
                if is_retryable_status(status) && attempt + 1 < ctx.provider_names.len() {
                    warn!(
                        "upstream returned {} request_id={} provider={}, trying next provider",
                        status, ctx.req_id, provider.name
                    );
                    last_error = Some(format!("provider {} returned {}", provider.name, status));
                    continue;
                }

                return match build_non_streaming_response(
                    state,
                    &upstream_body,
                    status,
                    route_info,
                    &ctx,
                    provider,
                ) {
                    Ok(response) => response,
                    Err(err) => conversion_error_response(err),
                };
            }
            Err(err) => {
                warn!(
                    "upstream request failed request_id={} provider={} error={}",
                    ctx.req_id, provider.name, err
                );
                last_error = Some(err);
                continue;
            }
        }
    }

    upstream_error_response(&last_error.unwrap_or_else(|| "all providers failed".into()))
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504 | 529)
}

fn strip_private_fields(body: &[u8]) -> Vec<u8> {
    if let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(obj) = val.as_object_mut() {
            obj.retain(|key, _| !key.starts_with('_'));
        }
        serde_json::to_vec(&val).unwrap_or_else(|_| body.to_vec())
    } else {
        body.to_vec()
    }
}

fn truncated_body(body: &[u8], max_len: usize) -> String {
    let s = String::from_utf8_lossy(body);
    let text = if s.len() <= max_len {
        s.into_owned()
    } else {
        format!(
            "{}...<truncated {} bytes>",
            &s[..max_len],
            s.len() - max_len
        )
    };
    sanitize_log_body(&text)
}

const SENSITIVE_KEYS: &[&str] = &["api_key", "apiKey", "authorization", "x-api-key", "secret"];

fn sanitize_log_body(text: &str) -> String {
    let mut result = text.to_string();
    for key in SENSITIVE_KEYS {
        let patterns = [format!("\"{key}\":\""), format!("\"{key}\": \"")];
        for pat in &patterns {
            while let Some(start) = result.find(pat.as_str()) {
                let val_start = start + pat.len();
                if let Some(end) = result[val_start..].find('"') {
                    let replacement = format!("{pat}[REDACTED]\"");
                    result = format!(
                        "{}{}{}",
                        &result[..start],
                        replacement,
                        &result[val_start + end + 1..]
                    );
                } else {
                    break;
                }
            }
        }
    }
    result
}

fn api_error_response(
    status: StatusCode,
    error_type: &str,
    message: impl std::fmt::Display,
) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": message.to_string(),
                "type": error_type,
                "code": error_type,
            }
        })),
    )
        .into_response()
}

fn auth_error_response(err: AuthError) -> Response {
    api_error_response(
        match err {
            AuthError::Missing | AuthError::Invalid => StatusCode::UNAUTHORIZED,
        },
        "authentication_error",
        err,
    )
}

fn upstream_error_response(err: &str) -> Response {
    error!("upstream error: {err}");
    api_error_response(StatusCode::BAD_GATEWAY, "upstream_error", err)
}

fn conversion_error_response(err: HandlerError) -> Response {
    api_error_response(StatusCode::INTERNAL_SERVER_ERROR, "conversion_error", err)
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
    use crate::config::{LoggingConfig, ProviderConfig, RouteConfig, ServerSettings};
    use std::collections::HashMap;

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
                model_routes: vec![],
                routes: vec![RouteConfig {
                    client_format: Format::Claude,
                    provider: "openai".into(),
                }],
                model_metadata: HashMap::new(),
                logging: LoggingConfig::default(),
            },
            client: Client::new(),
            usage_logger: None,
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
                model_routes: vec![],
                routes: vec![],
                model_metadata: HashMap::new(),
                logging: LoggingConfig::default(),
            },
            client: Client::new(),
            usage_logger: None,
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
        let body =
            br#"{"model":"claude-3","stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
        let route = RouteInfo {
            client_format: Format::Claude,
            path_streaming: false,
        };

        assert!(!is_streaming_request(body, route));
        assert_eq!(
            state
                .config
                .find_route(route.client_format)
                .unwrap()
                .provider,
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

    #[test]
    fn test_strip_private_fields_removes_underscore_keys() {
        let body = br#"{"model":"gpt-4.1","_stream_tokens":true,"messages":[]}"#;
        let result = strip_private_fields(body);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert!(parsed.get("model").is_some());
        assert!(parsed.get("messages").is_some());
        assert!(parsed.get("_stream_tokens").is_none());
    }

    #[test]
    fn test_strip_private_fields_preserves_normal_keys() {
        let body = br#"{"model":"gpt-4.1","stream":true}"#;
        let result = strip_private_fields(body);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed.get("model").unwrap().as_str().unwrap(), "gpt-4.1");
        assert_eq!(parsed.get("stream").unwrap().as_bool().unwrap(), true);
    }
}
