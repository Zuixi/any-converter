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
use crate::config::{RouteStrategy, ServerConfig};
use crate::model::{extract_model_from_body, patch_model_in_body, strip_private_fields};
use crate::namespace::{extract_namespace_map, patch_response_namespaces};
use crate::observability::utf8_prefix;
use crate::proxy::build_upstream_url_for_provider;
use crate::request_log::{RequestLogContext, RequestLogger};
use crate::route_strategy::order_provider_names;
use crate::router::RouteInfo;
use crate::usage::{self, UsageLogger, UsageRecord};

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub client: Client,
    pub usage_logger: Option<Arc<UsageLogger>>,
    pub request_logger: Option<Arc<RequestLogger>>,
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
    strategy: RouteStrategy,
    log_ctx: Option<RequestLogContext>,
}

/// Extract a best-effort client identifier from request headers.
///
/// Preference order: `user-agent` → `x-requested-with` → `x-stainless-*`.
fn extract_client_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or_else(|| {
            headers
                .get("x-requested-with")
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        })
        .or_else(|| {
            headers
                .keys()
                .find(|k| k.as_str().starts_with("x-stainless-"))
                .map(|k| k.as_str().to_string())
        })
}

/// Extract a session identifier from request headers.
///
/// Request and correlation ids are intentionally excluded because they usually
/// change per request and cannot identify a conversation.
fn extract_session_id(headers: &HeaderMap) -> Option<String> {
    const SESSION_HEADERS: &[&str] = &["x-session-id", "x-conversation-id"];
    for name in SESSION_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            return Some(value.to_string());
        }
    }
    None
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

    let stripped_body = strip_private_fields(body);

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

    let ns_map = extract_namespace_map(&stripped_body, route_info.client_format);
    let streaming = is_streaming_request(&stripped_body, route_info);
    let client_id = extract_client_id(headers);
    let session_id = extract_session_id(headers);

    let log_ctx = if state.request_logger.is_some() {
        Some(RequestLogContext {
            req_id,
            start_time,
            client_format: route_info.client_format,
            client_id: client_id.clone(),
            session_id: session_id.clone(),
            client_model: client_model.clone(),
            upstream_model: resolved.upstream_model.clone(),
            streaming,
            method: "POST".to_string(),
            path: path_for_route(route_info, gemini_model.unwrap_or(&client_model)),
            request_body: body.to_vec(),
            provider_name: String::new(),
            upstream_url: String::new(),
            upstream_request_body: Vec::new(),
        })
    } else {
        None
    };

    Ok((
        RequestContext {
            req_id,
            start_time,
            client_model,
            upstream_model: resolved.upstream_model,
            ns_map,
            streaming,
            provider_names: resolved.provider_names,
            strategy: resolved.strategy,
            log_ctx,
        },
        stripped_body,
    ))
}

fn path_for_route(route_info: RouteInfo, model: &str) -> String {
    match route_info.client_format {
        Format::OpenAIChat => "/v1/chat/completions".to_string(),
        Format::Claude => "/v1/messages".to_string(),
        Format::OpenAIResponses => "/v1/responses".to_string(),
        Format::Gemini => {
            if route_info.path_streaming {
                format!("/v1beta/models/{model}:streamGenerateContent")
            } else {
                format!("/v1beta/models/{model}:generateContent")
            }
        }
    }
}

fn convert_and_build_upstream(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    route_info: RouteInfo,
    ctx: &mut RequestContext,
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

    let url = build_upstream_url_for_provider(provider, &ctx.upstream_model, ctx.streaming);

    let auth_headers = auth::build_upstream_auth_headers_for_provider(provider);
    let upstream_headers = crate::proxy::build_upstream_headers(
        headers,
        &auth_headers,
        &state.config.server.pass_through_headers,
    );

    if state.config.logging.upstream_headers_log {
        info!(
            target: "upstream_headers",
            "upstream request headers request_id={} provider={} {}",
            ctx.req_id,
            provider.name,
            sanitize_headers_for_log(&upstream_headers)
        );
    }

    if let Some(ref mut log_ctx) = ctx.log_ctx {
        log_ctx.provider_name = provider.name.clone();
        log_ctx.upstream_url = url.clone();
        log_ctx.upstream_request_body = converted_body.clone();
    }

    Ok((converted_body, url, upstream_headers))
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

    if let Some(ref logger) = state.request_logger {
        if let Some(ref log_ctx) = ctx.log_ctx {
            crate::request_log::log_non_streaming(
                logger,
                log_ctx,
                upstream_body,
                &converted,
                status,
                provider.format,
                &state.config.logging.request_log,
            );
        }
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
    let (mut ctx, body) = match prepare_request(state, headers, body, route_info, gemini_model) {
        Ok(pair) => pair,
        Err(response) => return *response,
    };

    let mut last_error: Option<String> = None;
    let provider_names = order_provider_names(&ctx.provider_names, &ctx.strategy);

    for (attempt, provider_name) in provider_names.iter().enumerate() {
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

        let (converted_body, url, upstream_headers) =
            match convert_and_build_upstream(state, headers, &body, route_info, &mut ctx, provider)
            {
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
                &upstream_headers,
                headers,
                &state.config.server.pass_through_headers,
                provider.format,
                route_info.client_format,
                &ctx.ns_map,
                ctx.log_ctx.clone(),
                state.request_logger.clone(),
                state.config.logging.request_log.max_capture_bytes,
                state.config.logging.request_log.trace_enabled,
                state.config.logging.request_log.trace_max_preview_bytes,
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
            &upstream_headers,
            headers,
            &state.config.server.pass_through_headers,
        )
        .await
        {
            Ok((status, upstream_body)) => {
                if is_retryable_status(status) && attempt + 1 < provider_names.len() {
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

fn truncated_body(body: &[u8], max_len: usize) -> String {
    let s = String::from_utf8_lossy(body);
    let text = if s.len() <= max_len {
        s.into_owned()
    } else {
        let prefix = utf8_prefix(&s, max_len);
        format!("{}...<truncated {} bytes>", prefix, s.len() - prefix.len())
    };
    sanitize_log_body(&text)
}

const SENSITIVE_KEYS: &[&str] = &["api_key", "apiKey", "authorization", "x-api-key", "secret"];

/// Header names whose values are redacted when logging upstream requests.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "proxy-authorization",
];

fn sanitize_headers_for_log(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let name_lower = name.to_ascii_lowercase();
            if SENSITIVE_HEADERS.contains(&name_lower.as_str()) {
                format!("{name}=[REDACTED]")
            } else {
                format!("{name}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::config::{
        LoggingConfig, PassThroughHeadersConfig, ProviderConfig, RouteConfig, ServerSettings,
    };
    use axum::http::HeaderValue;
    use std::collections::HashMap;

    fn sample_state() -> AppState {
        AppState {
            config: ServerConfig {
                server: ServerSettings {
                    host: "127.0.0.1".into(),
                    port: 8080,
                    api_key: None,
                    pass_through_headers: PassThroughHeadersConfig::default(),
                },
                providers: vec![ProviderConfig {
                    name: "openai".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://api.openai.com".into(),
                    api_key: "sk-test".into(),
                    model_map: [("*".into(), "gpt-4.1".into())].into(),
                    endpoints: Default::default(),
                    auth: Default::default(),
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
            request_logger: None,
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

    #[test]
    fn request_and_correlation_ids_are_not_session_ids() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("request-1"));
        headers.insert(
            "x-correlation-id",
            HeaderValue::from_static("correlation-1"),
        );

        assert_eq!(extract_session_id(&headers), None);
    }

    #[tokio::test]
    async fn test_missing_route_returns_404() {
        let state = Arc::new(AppState {
            config: ServerConfig {
                server: ServerSettings {
                    host: "127.0.0.1".into(),
                    port: 8080,
                    api_key: None,
                    pass_through_headers: PassThroughHeadersConfig::default(),
                },
                providers: vec![],
                model_routes: vec![],
                routes: vec![],
                model_metadata: HashMap::new(),
                logging: LoggingConfig::default(),
            },
            client: Client::new(),
            usage_logger: None,
            request_logger: None,
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
}
