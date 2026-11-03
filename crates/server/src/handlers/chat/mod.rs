//! Model requests: routing, conversion, account selection, failover and the
//! plugin transport.

use std::collections::HashMap;
use std::sync::Arc;

use any_converter_core::convert::Format;
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use log::{error, info, warn};
use serde_json::Value;

use crate::auth;
use crate::config::{ProviderConfig, RouteStrategy, UpstreamApi};
use crate::credentials::{Allowance, ApiKeySource, TokenSource};
use crate::model::{extract_model_from_body, patch_model_in_body};
use crate::provider_pool::order_accounts;
use crate::request_log::RequestLogContext;
use crate::route_strategy::order_provider_names;
use crate::router::RouteInfo;

use super::accounts::{QUOTA_SHARE, cached_account_allowance};
use super::{
    AppState, api_error_response, auth_error_response, conversion_error_response,
    upstream_error_response,
};

mod failover;
mod forward;
mod plan;
mod plugin;

pub(crate) use failover::{AuthStep, classify_account_auth, is_retryable_status, take_error_body};
pub(crate) use forward::{AccountStep, build_non_streaming_response, convert_and_build_upstream};
// `extract_client_id` / `extract_session_id` are re-exported for the
// tests below (mod_test) only, which the lib build can't see.
pub(crate) use plan::{estimate_input_tokens, prepare_request, resolve_upstream_api};
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use plan::{extract_client_id, extract_session_id};
pub(crate) use plugin::{PluginOutcome, forward_via_plugin};

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

/// Handle Anthropic `count_tokens`: forward to Claude-native upstreams,
/// otherwise answer with a local byte-length estimate.
pub async fn handle_count_tokens(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    if let Err(err) = auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
    {
        return auth_error_response(err);
    }

    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(err) => {
            return api_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("invalid JSON body: {err}"),
            );
        }
    };

    let client_model = extract_model_from_body(&body).unwrap_or_default();

    if let Some(response) = forward_count_tokens(&state, &body, &client_model).await {
        return response;
    }

    Json(serde_json::json!({ "input_tokens": estimate_input_tokens(&value) })).into_response()
}

/// Forward `count_tokens` to the resolved provider when it natively speaks Claude.
///
/// Returns `None` when the caller should fall back to the local estimate.
///
/// Only the static `api_key` path is supported here on purpose: the Gemini
/// Code Assist wire has no count_tokens endpoint, so account-auth providers
/// fall back to the local byte-length estimate.
async fn forward_count_tokens(
    state: &AppState,
    body: &[u8],
    client_model: &str,
) -> Option<Response> {
    let resolved = state.config.resolve_provider(client_model)?;
    let provider = resolved
        .provider_names
        .first()
        .and_then(|name| state.config.find_provider(name))?;
    if provider.format != Format::Claude {
        return None;
    }

    let mut forward_body = body.to_vec();
    if resolved.upstream_model != client_model {
        if let Err(err) =
            patch_model_in_body(&mut forward_body, Format::Claude, &resolved.upstream_model)
        {
            warn!(
                "failed to patch model for count_tokens provider={} error={err}",
                provider.name
            );
        }
    }

    let url = format!(
        "{}/v1/messages/count_tokens",
        provider.base_url.trim_end_matches('/')
    );

    let mut upstream_headers = ApiKeySource::for_provider(provider)
        .auth_headers()
        .await
        .unwrap_or_default();
    if !upstream_headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-version"))
    {
        upstream_headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
    }
    upstream_headers.push(("content-type".to_string(), "application/json".to_string()));

    let mut request = state.client.post(&url);
    for (name, value) in upstream_headers {
        request = request.header(name, value);
    }

    match request.body(forward_body).send().await {
        Ok(resp) if resp.status().is_success() => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            match resp.bytes().await {
                Ok(bytes) => Some(
                    Response::builder()
                        .status(status)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(bytes))
                        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
                ),
                Err(err) => {
                    warn!(
                        "count_tokens upstream body read failed provider={} error={err}, using local estimate",
                        provider.name
                    );
                    None
                }
            }
        }
        Ok(resp) => {
            warn!(
                "count_tokens upstream returned {} provider={}, using local estimate",
                resp.status(),
                provider.name
            );
            None
        }
        Err(err) => {
            warn!(
                "count_tokens upstream request failed provider={} error={err}, using local estimate",
                provider.name
            );
            None
        }
    }
}

pub(crate) struct RequestContext {
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

/// One attempt to serve a request from one provider — and, for an account
/// provider, one of its seats. Bundled once so the stages below take one
/// reference instead of threading state/provider/api/route_info through
/// every signature. The mutable `RequestContext` is passed separately: it
/// is the only thing any stage writes, so keeping it out here avoids
/// borrowing it for the whole loop body.
#[derive(Clone, Copy)]
pub(crate) struct Attempt<'a> {
    pub state: &'a AppState,
    pub provider: &'a ProviderConfig,
    pub api: UpstreamApi,
    pub route_info: RouteInfo,
}

/// Where the failover loop stands: which target it is on, whether this
/// attempt already spent its one refresh-and-retry, and what to report if
/// nothing answers.
pub(crate) struct Failover {
    pub attempt: usize,
    pub expired_retried: Option<usize>,
    pub last_error: Option<String>,
    pub plan_len: usize,
}

impl Failover {
    pub(crate) fn new(plan_len: usize) -> Self {
        Self {
            attempt: 0,
            expired_retried: None,
            last_error: None,
            plan_len,
        }
    }
    /// Whether another target remains after this one.
    pub(crate) fn has_next(&self) -> bool {
        self.attempt + 1 < self.plan_len
    }
    /// Whether this attempt already took its one `token_expired` round.
    pub(crate) fn retried(&self) -> bool {
        self.expired_retried == Some(self.attempt)
    }
}

/// The attempts one request will make, in order: every provider's seats
/// flattened into one list (P9/P17).
///
/// A provider contributes its accounts in the user's saved order, with the
/// ones past their quota line moved last rather than reordered; a provider
/// with no accounts contributes a single api_key slot. A revoked seat is
/// dropped here — before the body is converted for it, which is a full JSON
/// parse and rewrite that a revoked first seat would otherwise pay for on
/// every request. `Err` carries the response to return as it is.
pub(crate) async fn build_plan(
    state: &AppState,
    ctx: &RequestContext,
    provider_names: &[String],
) -> Result<(Vec<(ProviderConfig, UpstreamApi)>, Option<String>), Response> {
    // P9: flatten providers × accounts into one stable attempt plan. Each
    // provider contributes its accounts in the user's order, with the ones
    // past their quota line moved last rather than reordered
    // (provider_pool); a provider with no accounts contributes a single
    // api_key slot. The same account is never tried twice in one request.
    let mut plan: Vec<(crate::config::ProviderConfig, UpstreamApi)> = Vec::new();
    let mut plan_error: Option<String> = None;
    for provider_name in provider_names {
        let provider = match state.config.find_provider(provider_name) {
            Some(p) => p,
            None => {
                error!(
                    "provider not found: {} request_id={}",
                    provider_name, ctx.req_id
                );
                plan_error = Some(format!("provider not found: {provider_name}"));
                continue;
            }
        };

        // The user's saved order comes first (P17): it is authoritative, and
        // a seat it doesn't name keeps its configured place at the end.
        let saved_order = crate::provider_pool::saved_order(
            &crate::config::secrets_path(),
            provider
                .account_ids()
                .first()
                .and_then(|id| id.split(':').next())
                .unwrap_or(""),
        );
        let ordered_ids =
            crate::provider_pool::apply_order(&provider.account_ids(), saved_order.as_ref());
        let ids: Vec<&str> = ordered_ids.iter().map(String::as_str).collect();
        let mut allowances: HashMap<String, Allowance> = HashMap::new();
        // Reading quota costs an upstream call (for Copilot a session
        // exchange): with a single account there is nothing to reorder,
        // so the reading is skipped (magpie reads quotas to rank seats).
        if ids.len() > 1 {
            // Routing never waits on a quota read (magpie: allowances are
            // read in the background and "never waited for"). A cached,
            // fresh value decides the order; a stale one is refreshed off
            // the request path, and an account with nothing cached counts
            // as having room, so a cold start keeps the user's order.
            for account_id in &ids {
                if let Some(allowance) = cached_account_allowance(state, provider, account_id) {
                    allowances.insert((*account_id).to_string(), allowance);
                }
            }
        }
        let candidates: Vec<Option<String>> = if ids.is_empty() {
            vec![None]
        } else {
            let allowance_map: HashMap<&str, Allowance> = allowances
                .iter()
                .map(|(account_id, allowance)| (account_id.as_str(), allowance.clone()))
                .collect();
            order_accounts(
                &ids,
                &allowance_map,
                &ctx.upstream_model,
                QUOTA_SHARE,
                std::time::SystemTime::now(),
            )
            .into_iter()
            .map(|account_id| Some(account_id.to_string()))
            .collect()
        };

        for account in candidates {
            // A revoked account is dropped before the body is converted for
            // it: conversion is a full JSON parse/rewrite, and a pool whose
            // first seat is revoked would otherwise pay for it on every
            // request just to discover the account can't serve.
            if let Some(id) = account.as_deref() {
                if state.accounts.is_revoked(id) {
                    warn!(
                        "skipping revoked account request_id={} account={id}",
                        ctx.req_id
                    );
                    continue;
                }
            }
            let mut effective = provider.clone();
            effective.account = account;
            let api = match resolve_upstream_api(state, &effective, &ctx.upstream_model).await {
                Ok(api) => api,
                Err(response) => return Err(response),
            };
            plan.push((effective, api));
        }
    }

    Ok((plan, plan_error))
}

pub(crate) async fn process_request(
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

    let provider_names = order_provider_names(&ctx.provider_names, &ctx.strategy);

    let (plan, plan_error) = match build_plan(state, &ctx, &provider_names).await {
        Ok(built) => built,
        Err(response) => return response,
    };

    // Failover with account-aware 401/403 handling. `fo.expired_retried`
    // remembers which attempt already consumed its single `token_expired`
    // → refresh → retry round, so an account that keeps answering
    // `token_expired` cannot loop forever.
    let mut fo = Failover::new(plan.len());
    if fo.last_error.is_none() {
        fo.last_error = plan_error;
    }

    while fo.attempt < plan.len() {
        let (provider, api) = &plan[fo.attempt];
        let att = Attempt {
            state,
            provider,
            api: *api,
            route_info,
        };

        let (converted_body, url, upstream_headers) =
            match convert_and_build_upstream(&att, &mut ctx, headers, &body).await {
                Ok(AccountStep::Ready {
                    converted_body,
                    url,
                    upstream_headers,
                }) => (converted_body, url, upstream_headers),
                Ok(AccountStep::Unavailable { response }) => {
                    // The account cannot serve this request (not signed in /
                    // revoked / Copilot refused): try the next account; the
                    // guidance below goes back only when no target remains.
                    fo.last_error = Some(format!(
                        "provider {} account {} unavailable",
                        provider.name,
                        provider.account.as_deref().unwrap_or("")
                    ));
                    if fo.has_next() {
                        fo.attempt += 1;
                        continue;
                    }
                    return response;
                }
                Err(response) => return *response,
            };

        // Code Assist responses are enveloped in a response wrapper that the
        // proxy unwraps to plain Gemini before conversion.
        let response_format = api.to_format();

        info!(
            "forwarding request request_id={} client_format={} provider={} upstream_model={} streaming={} attempt={}",
            ctx.req_id,
            route_info.client_format,
            provider.name,
            ctx.upstream_model,
            ctx.streaming,
            fo.attempt + 1
        );

        // Plugin transport (P6.2): the request goes through the plugin's
        // `fetch` hook instead of the local HTTP client. Everything up to
        // here is shared — the body is already converted to the per-model
        // wire protocol and the plugin URL is computed inside the branch.
        if let Some(plugin_ref) = &provider.plugin {
            match forward_via_plugin(&att, &mut ctx, plugin_ref, upstream_headers, converted_body)
                .await
            {
                PluginOutcome::Answered(response, status) => {
                    // The same retry rule the http path applies: a retryable
                    // status moves to the next account/provider while one
                    // remains. 401/403 pass through unchanged — the plugin
                    // owns its credentials, so no local account refresh
                    // applies.
                    if is_retryable_status(status) && fo.has_next() {
                        warn!(
                            "plugin provider returned {} request_id={} provider={}, trying next account/provider",
                            status, ctx.req_id, provider.name
                        );
                        fo.last_error =
                            Some(format!("provider {} returned {}", provider.name, status));
                        fo.attempt += 1;
                        continue;
                    }
                    return response;
                }
                PluginOutcome::Failed { error } => {
                    warn!(
                        "plugin forward failed request_id={} provider={} error={error}",
                        ctx.req_id, provider.name
                    );
                    fo.last_error = Some(error);
                    fo.attempt += 1;
                    continue;
                }
            }
        }

        if ctx.streaming {
            match crate::proxy::forward_streaming(
                &state.client,
                &url,
                converted_body,
                &upstream_headers,
                headers,
                &state.config.server.pass_through_headers,
                response_format,
                route_info.client_format,
                *api,
                &ctx.ns_map,
                ctx.log_ctx.clone(),
                state.request_logger.clone(),
                state.config.logging.request_log.max_capture_bytes,
                state.config.logging.request_log.trace_enabled,
                state.config.logging.request_log.trace_max_preview_bytes,
            )
            .await
            {
                Ok((response, status)) => {
                    // Account-aware 401/403 semantics: the proxy buffers the
                    // whole error body on non-success statuses, so it can be
                    // classified here; the response is rebuilt so it can
                    // still be returned unchanged when nothing applies.
                    let response = match provider.account.clone() {
                        Some(account_id)
                            if matches!(
                                status,
                                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                            ) =>
                        {
                            let (rebuilt, text) = take_error_body(response).await;
                            match classify_account_auth(&att, &ctx, &mut fo, &account_id, &text)
                                .await
                            {
                                Ok(AuthStep::RetrySame) => continue,
                                Ok(AuthStep::Next) => {
                                    fo.attempt += 1;
                                    continue;
                                }
                                Ok(AuthStep::FallThrough) => rebuilt,
                                Err(response) => return response,
                            }
                        }
                        _ => response,
                    };

                    // the same retry rule the non-streaming path applies: a
                    // retryable status moves to the next account/provider
                    // while one remains; otherwise the client sees the
                    // upstream answer
                    if is_retryable_status(status) && fo.has_next() {
                        warn!(
                            "upstream returned {} request_id={} provider={}, trying next account/provider",
                            status, ctx.req_id, provider.name
                        );
                        fo.last_error =
                            Some(format!("provider {} returned {}", provider.name, status));
                        fo.attempt += 1;
                        continue;
                    }
                    return response;
                }
                Err(err) => {
                    warn!(
                        "streaming forward failed request_id={} provider={} error={}",
                        ctx.req_id, provider.name, err
                    );
                    fo.last_error = Some(err);
                    fo.attempt += 1;
                    continue;
                }
            }
        }

        match crate::proxy::forward_non_streaming(
            &state.client,
            &url,
            converted_body,
            &upstream_headers,
            headers,
            &state.config.server.pass_through_headers,
            *api,
        )
        .await
        {
            Ok((status, upstream_body)) => {
                // Account-aware 401/403 semantics (only when the provider
                // is signed in through an account).
                if let Some(account_id) = provider.account.clone() {
                    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                        let text = String::from_utf8_lossy(&upstream_body);
                        match classify_account_auth(&att, &ctx, &mut fo, &account_id, &text).await {
                            Ok(AuthStep::RetrySame) => continue,
                            Ok(AuthStep::Next) => {
                                fo.attempt += 1;
                                continue;
                            }
                            Ok(AuthStep::FallThrough) => {}
                            Err(response) => return response,
                        }
                    }
                }

                if is_retryable_status(status) && fo.has_next() {
                    warn!(
                        "upstream returned {} request_id={} provider={}, trying next account/provider",
                        status, ctx.req_id, provider.name
                    );
                    fo.last_error = Some(format!("provider {} returned {}", provider.name, status));
                    fo.attempt += 1;
                    continue;
                }

                return match build_non_streaming_response(
                    &att,
                    &ctx,
                    &upstream_body,
                    status,
                    response_format,
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
                fo.last_error = Some(err);
                fo.attempt += 1;
                continue;
            }
        }
    }

    upstream_error_response(
        &fo.last_error
            .unwrap_or_else(|| "all providers failed".into()),
    )
}
