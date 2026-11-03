//! From a request to a plan: the conversation's ids, whether it streams,
//! which client format it came in and which upstream API it goes to.

use crate::auth;
use any_converter_core::convert::Format;
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use log::{error, info, warn};
use serde_json::Value;

use crate::agent_id::agent_of;
use crate::config::UpstreamApi;
use crate::namespace::extract_namespace_map;
use crate::router::RouteInfo;

use super::super::{api_error_response, auth_error_response, is_streaming_request};
use super::{AppState, RequestContext};
use crate::model::{extract_model_from_body, strip_private_fields};
use crate::request_log::RequestLogContext;

/// Rough input-token estimate: total UTF-8 bytes of every string in the body / 4.
pub(crate) fn estimate_input_tokens(body: &Value) -> u64 {
    fn string_bytes(value: &Value) -> u64 {
        match value {
            Value::String(s) => s.len() as u64,
            Value::Array(items) => items.iter().map(string_bytes).sum(),
            Value::Object(map) => map.values().map(string_bytes).sum(),
            _ => 0,
        }
    }
    string_bytes(body) / 4
}

/// Extract a best-effort client identifier from request headers.
///
/// Preference order: `user-agent` → `x-requested-with` → `x-stainless-*`.
/// The user-agent is normalized to a known agent id (see [`crate::agent_id`]).
pub(crate) fn extract_client_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(agent_of)
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
pub(crate) fn extract_session_id(headers: &HeaderMap) -> Option<String> {
    const SESSION_HEADERS: &[&str] = &["x-session-id", "x-conversation-id"];
    for name in SESSION_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            return Some(value.to_string());
        }
    }
    None
}

pub(crate) fn prepare_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    route_info: RouteInfo,
    gemini_model: Option<&str>,
    agent_keys: &[String],
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

    // Global or per-agent gateway key: validate synchronously here; the
    // async attribution/lazy-sync side (`registry.authenticate`) runs in
    // `process_request` before this function is reached, which also passes
    // the key snapshot in (no lock is taken on this path).
    auth::validate_client_keys(
        state.config.server.api_key.as_deref(),
        agent_keys.iter().map(String::as_str),
        auth_header,
    )
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
        .resolve_provider(&client_model)
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

pub(crate) fn path_for_route(route_info: RouteInfo, model: &str) -> String {
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

/// The upstream API a request for `model` is shaped as.
///
/// Plugin providers resolve per model: the plugin's model list is consulted
/// and the matched model's `npm` package selects the wire protocol
/// ([`UpstreamApi::from_npm`]); a model the plugin does not list falls back
/// to Chat. Every other provider resolves through [`ProviderConfig::api_for`]
/// (per-model dispatch for Copilot accounts, P7).
pub(crate) async fn resolve_upstream_api(
    state: &AppState,
    provider: &crate::config::ProviderConfig,
    model: &str,
) -> Result<UpstreamApi, Box<Response>> {
    let Some(plugin_ref) = &provider.plugin else {
        return Ok(provider.api_for(model));
    };
    let account = provider.account.clone().unwrap_or_default();
    let models = state
        .hosts
        .models_for(plugin_ref, &account)
        .await
        .map_err(|err| {
            error!(
                "plugin models failed provider={} error={err}",
                provider.name
            );
            api_error_response(
                StatusCode::BAD_GATEWAY,
                "plugin_error",
                format!("plugin models: {err}"),
            )
        })?;
    for plugin_model in &models {
        if plugin_model.id == model
            || (!plugin_model.api_id.is_empty() && plugin_model.api_id == model)
        {
            return Ok(UpstreamApi::from_npm(&plugin_model.npm));
        }
    }
    warn!(
        "plugin model `{model}` not listed by provider {}, falling back to Chat",
        provider.name
    );
    Ok(UpstreamApi::Chat)
}
