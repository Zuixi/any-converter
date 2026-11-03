//! Model requests: routing, conversion, account selection, failover and the
//! plugin transport.

use super::accounts::{QUOTA_SHARE, cached_account_allowance};
use super::*;

mod plugin;

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

/// The upstream API a request for `model` is shaped as.
///
/// Plugin providers resolve per model: the plugin's model list is consulted
/// and the matched model's `npm` package selects the wire protocol
/// ([`UpstreamApi::from_npm`]); a model the plugin does not list falls back
/// to Chat. Every other provider resolves through [`ProviderConfig::api_for`]
/// (per-model dispatch for Copilot accounts, P7).
async fn resolve_upstream_api(
    state: &AppState,
    provider: &crate::config::ProviderConfig,
    model: &str,
) -> Result<UpstreamApi, Response> {
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

/// Outcome of building the upstream request for one account attempt (P9).
enum AccountStep {
    /// The converted body, URL and headers are ready to send.
    Ready {
        converted_body: Vec<u8>,
        url: String,
        upstream_headers: Vec<(String, String)>,
    },
    /// The account cannot serve this request (not signed in, revoked, or
    /// its Copilot session was refused): skip it and try the next one.
    /// `response` is what to send when no other account/provider remains.
    Unavailable { response: Response },
}

async fn convert_and_build_upstream(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    route_info: RouteInfo,
    ctx: &mut RequestContext,
    provider: &crate::config::ProviderConfig,
    api: UpstreamApi,
) -> Result<AccountStep, Box<Response>> {
    // Code Assist always speaks Gemini on the wire: conversion targets Gemini
    // (not `provider.format`) and the body rides inside the request envelope.
    let to_format = api.to_format();

    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            "request_original request_id={} client_format={} provider_format={} body={}",
            ctx.req_id, route_info.client_format, to_format, truncated_body(body, 4096)
        );
    }

    let converted_body = match safe_convert_request(body, route_info.client_format, to_format) {
        Ok(mut bytes) => {
            if state.config.logging.conversion_log {
                debug!(
                    target: "conversion",
                    "request_converted request_id={} body={}",
                    ctx.req_id, truncated_body(&bytes, 4096)
                );
            }
            if api == UpstreamApi::CodeAssist {
                // The model id rides in the Code Assist envelope, not in the
                // Gemini body or URL.
                crate::wire::wrap_code_assist(&bytes, &ctx.upstream_model).map_err(|err| {
                    error!(
                        "code assist envelope failed request_id={} error={err}",
                        ctx.req_id
                    );
                    Box::new(conversion_error_response(HandlerError::Envelope(err)))
                })?
            } else {
                if let Err(err) =
                    patch_model_in_body(&mut bytes, api.to_format(), &ctx.upstream_model)
                {
                    error!("failed to patch model: {err} request_id={}", ctx.req_id);
                }
                bytes
            }
        }
        Err(err) => {
            error!(
                "request conversion failed request_id={} from={} to={} error={}",
                ctx.req_id, route_info.client_format, to_format, err
            );
            return Err(Box::new(conversion_error_response(err)));
        }
    };

    // Copilot accounts (copilot:...) resolve the GitHub OAuth token and
    // exchange it for a short-lived Copilot session (magpie copilotToken)
    // before building the request: the session supplies both the API base
    // URL and the auth headers.
    let copilot_session = if provider.is_copilot_account() {
        let account_id = provider.account.clone().unwrap_or_default();
        let account = match state.accounts.resolve(&account_id).await {
            Ok(account) => account,
            Err(err) => {
                warn!(
                    "account resolution failed request_id={} account={} error={err}",
                    ctx.req_id, account_id
                );
                return Ok(AccountStep::Unavailable {
                    response: api_error_response(
                        StatusCode::BAD_GATEWAY,
                        "account_error",
                        format!("account {account_id}: {err}"),
                    ),
                });
            }
        };
        match state.copilot.session(&account.access_token).await {
            Ok(session) => Some(session),
            Err(CopilotError::Refused(message)) => {
                warn!(
                    "copilot session refused request_id={} account={} error={message}",
                    ctx.req_id, account_id
                );
                return Ok(AccountStep::Unavailable {
                    response: api_error_response(
                        StatusCode::BAD_GATEWAY,
                        "account_error",
                        format!(
                            "Copilot account {account_id}: {message}; please re-login (重新登录)"
                        ),
                    ),
                });
            }
            Err(CopilotError::Exchange(message)) => {
                warn!(
                    "copilot session exchange failed request_id={} account={} error={message}",
                    ctx.req_id, account_id
                );
                return Ok(AccountStep::Unavailable {
                    response: api_error_response(
                        StatusCode::BAD_GATEWAY,
                        "account_error",
                        format!("Copilot account {account_id}: {message}"),
                    ),
                });
            }
        }
    } else {
        None
    };

    let url = if let Some(session) = &copilot_session {
        // The session's `endpoints.api` overrides `provider.base_url`
        // (defaulting to the public Copilot base); the path follows the
        // P6.3 rule of stripping the leading `/v1` (magpie api.path()).
        let base = if session.api_base.is_empty() {
            COPILOT_DEFAULT_API_BASE
        } else {
            session.api_base.as_str()
        };
        let path = api.path(&ctx.upstream_model, ctx.streaming);
        let path = path.strip_prefix("/v1").unwrap_or(&path);
        format!("{}{path}", base.trim_end_matches('/'))
    } else if provider.plugin.is_some() {
        // Plugin transport: the URL is `loader.base_url + api.path(...)`,
        // computed on the plugin branch of `process_request` where the
        // loader options are known. The CodeAssist envelope above still
        // applies — only the URL moves.
        String::new()
    } else {
        match api {
            UpstreamApi::Responses
                if provider
                    .account_ids()
                    .first()
                    .is_some_and(|id| id.starts_with("chatgpt-api:")) =>
            {
                crate::proxy::build_upstream_url(
                    Format::OpenAIResponses,
                    &provider.chatgpt_api_base(),
                    &ctx.upstream_model,
                    ctx.streaming,
                )
            }
            UpstreamApi::CodeAssist => crate::wire::build_url(
                UpstreamApi::CodeAssist,
                // Antigravity's generation host is the daily one; a provider
                // that names its own base keeps it (magpie app.base).
                &provider.code_assist_base(),
                &ctx.upstream_model,
                ctx.streaming,
            ),
            _ => build_upstream_url_for_provider(provider, &ctx.upstream_model, ctx.streaming),
        }
    };

    // Account-based OAuth signing wins over the static API key path: when
    // `provider.account` is set, `provider.api_key` is ignored entirely.
    // Plugin providers sign their own requests (the loader owns the base
    // URL, API key and headers), so the host never builds an Authorization
    // header for them; provider-level extra headers still ride along.
    let auth_headers: Vec<(String, String)> = if let Some(session) = &copilot_session {
        // Copilot wire: Bearer session token plus the Copilot client
        // headers and API version (magpie copilotAPIHeaders).
        copilot_api_headers(&session.token)
    } else if provider.plugin.is_some() {
        provider
            .auth
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    } else if let Some(account_id) = &provider.account {
        let account = match state.accounts.resolve(account_id).await {
            Ok(account) => account,
            Err(err) => {
                warn!(
                    "account resolution failed request_id={} account={} error={err}",
                    ctx.req_id, account_id
                );
                return Ok(AccountStep::Unavailable {
                    response: api_error_response(
                        StatusCode::BAD_GATEWAY,
                        "account_error",
                        format!("account {account_id}: {err}"),
                    ),
                });
            }
        };
        let mut headers = vec![(
            "Authorization".to_string(),
            format!("Bearer {}", account.access_token),
        )];
        headers.extend(
            provider
                .auth
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        headers
    } else {
        ApiKeySource::for_provider(provider)
            .auth_headers()
            .await
            .unwrap_or_default()
    };
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

    Ok(AccountStep::Ready {
        converted_body,
        url,
        upstream_headers,
    })
}

fn build_non_streaming_response(
    state: &AppState,
    upstream_body: &[u8],
    status: reqwest::StatusCode,
    route_info: RouteInfo,
    ctx: &RequestContext,
    provider: &crate::config::ProviderConfig,
    from_format: Format,
) -> Result<Response, HandlerError> {
    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            "response_original request_id={} status={} body={}",
            ctx.req_id, status, truncated_body(upstream_body, 4096)
        );
    }

    let mut converted =
        safe_convert_response(upstream_body, from_format, route_info.client_format)?;

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
                from_format,
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

    let mut last_error: Option<String> = None;
    let provider_names = order_provider_names(&ctx.provider_names, &ctx.strategy);

    // P9: flatten providers × accounts into one stable attempt plan. Each
    // provider contributes its accounts in the user's order, with the ones
    // past their quota line moved last rather than reordered
    // (provider_pool); a provider with no accounts contributes a single
    // api_key slot. The same account is never tried twice in one request.
    let mut plan: Vec<(crate::config::ProviderConfig, UpstreamApi)> = Vec::new();
    for provider_name in &provider_names {
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
                Err(response) => return response,
            };
            plan.push((effective, api));
        }
    }

    // Failover with account-aware 401/403 handling. `expired_retried`
    // remembers which attempt already consumed its single `token_expired`
    // → refresh → retry round, so an account that keeps answering
    // `token_expired` cannot loop forever.
    let mut attempt: usize = 0;
    let mut expired_retried: Option<usize> = None;
    let has_next = |attempt: usize| attempt + 1 < plan.len();

    while attempt < plan.len() {
        let (provider, api) = &plan[attempt];

        let (converted_body, url, upstream_headers) = match convert_and_build_upstream(
            state, headers, &body, route_info, &mut ctx, provider, *api,
        )
        .await
        {
            Ok(AccountStep::Ready {
                converted_body,
                url,
                upstream_headers,
            }) => (converted_body, url, upstream_headers),
            Ok(AccountStep::Unavailable { response }) => {
                // The account cannot serve this request (not signed in /
                // revoked / Copilot refused): try the next account; the
                // guidance below goes back only when no target remains.
                last_error = Some(format!(
                    "provider {} account {} unavailable",
                    provider.name,
                    provider.account.as_deref().unwrap_or("")
                ));
                if has_next(attempt) {
                    attempt += 1;
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
            attempt + 1
        );

        // Plugin transport (P6.2): the request goes through the plugin's
        // `fetch` hook instead of the local HTTP client. Everything up to
        // here is shared — the body is already converted to the per-model
        // wire protocol and the plugin URL is computed inside the branch.
        if let Some(plugin_ref) = &provider.plugin {
            match forward_via_plugin(
                state,
                plugin_ref,
                provider,
                *api,
                upstream_headers,
                converted_body,
                &mut ctx,
                route_info,
            )
            .await
            {
                PluginOutcome::Answered(response, status) => {
                    // The same retry rule the http path applies: a retryable
                    // status moves to the next account/provider while one
                    // remains. 401/403 pass through unchanged — the plugin
                    // owns its credentials, so no local account refresh
                    // applies.
                    if is_retryable_status(status) && has_next(attempt) {
                        warn!(
                            "plugin provider returned {} request_id={} provider={}, trying next account/provider",
                            status, ctx.req_id, provider.name
                        );
                        last_error =
                            Some(format!("provider {} returned {}", provider.name, status));
                        attempt += 1;
                        continue;
                    }
                    return response;
                }
                PluginOutcome::Failed { error } => {
                    warn!(
                        "plugin forward failed request_id={} provider={} error={error}",
                        ctx.req_id, provider.name
                    );
                    last_error = Some(error);
                    attempt += 1;
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
                            match classify_account_auth(
                                state,
                                &ctx,
                                provider,
                                &account_id,
                                &text,
                                expired_retried == Some(attempt),
                                attempt,
                                has_next(attempt),
                                &mut expired_retried,
                                &mut last_error,
                            )
                            .await
                            {
                                Ok(AuthStep::RetrySame) => continue,
                                Ok(AuthStep::Next) => {
                                    attempt += 1;
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
                    if is_retryable_status(status) && has_next(attempt) {
                        warn!(
                            "upstream returned {} request_id={} provider={}, trying next account/provider",
                            status, ctx.req_id, provider.name
                        );
                        last_error =
                            Some(format!("provider {} returned {}", provider.name, status));
                        attempt += 1;
                        continue;
                    }
                    return response;
                }
                Err(err) => {
                    warn!(
                        "streaming forward failed request_id={} provider={} error={}",
                        ctx.req_id, provider.name, err
                    );
                    last_error = Some(err);
                    attempt += 1;
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
                        match classify_account_auth(
                            state,
                            &ctx,
                            provider,
                            &account_id,
                            &text,
                            expired_retried == Some(attempt),
                            attempt,
                            has_next(attempt),
                            &mut expired_retried,
                            &mut last_error,
                        )
                        .await
                        {
                            Ok(AuthStep::RetrySame) => continue,
                            Ok(AuthStep::Next) => {
                                attempt += 1;
                                continue;
                            }
                            Ok(AuthStep::FallThrough) => {}
                            Err(response) => return response,
                        }
                    }
                }

                if is_retryable_status(status) && has_next(attempt) {
                    warn!(
                        "upstream returned {} request_id={} provider={}, trying next account/provider",
                        status, ctx.req_id, provider.name
                    );
                    last_error = Some(format!("provider {} returned {}", provider.name, status));
                    attempt += 1;
                    continue;
                }

                return match build_non_streaming_response(
                    state,
                    &upstream_body,
                    status,
                    route_info,
                    &ctx,
                    provider,
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
                last_error = Some(err);
                attempt += 1;
                continue;
            }
        }
    }

    upstream_error_response(&last_error.unwrap_or_else(|| "all providers failed".into()))
}

/// Failover-loop action for one provider attempt after an account-aware
/// 401/403.
enum AuthStep {
    /// Retry the same provider without consuming failover quota.
    RetrySame,
    /// Move to the next provider.
    Next,
    /// No account semantics applied: keep the existing status handling.
    FallThrough,
}

/// Account-aware handling of an upstream 401/403 for providers with
/// `account` set: one refresh on `token_expired`, a persisted marker in
/// `revoked.json` on `token_revoked` / `invalid_grant` /
/// `refresh_token_reused`, and the corresponding loop step. An Unknown
/// 401 moves on when another account/provider target remains (P9).
///
/// Only the account id and the verdict are logged — never the response
/// body, which can echo tokens. `Err(response)` ends the request right
/// here (revoked account without another target).
#[allow(clippy::too_many_arguments)]
async fn classify_account_auth(
    state: &AppState,
    ctx: &RequestContext,
    provider: &crate::config::ProviderConfig,
    account_id: &str,
    text: &str,
    already_retried: bool,
    attempt: usize,
    has_next: bool,
    expired_retried: &mut Option<usize>,
    last_error: &mut Option<String>,
) -> Result<AuthStep, Response> {
    let step = match classify_401(text) {
        AuthErr401::Expired if !already_retried => {
            info!(
                "upstream returned token_expired request_id={} provider={} account={account_id}, refreshing once",
                ctx.req_id, provider.name
            );
            match state.accounts.force_refresh(account_id).await {
                Ok(_) => AuthStep::RetrySame,
                Err(err) => {
                    warn!(
                        "refresh after token_expired failed request_id={} provider={} account={account_id} error={err}, trying next account/provider",
                        ctx.req_id, provider.name
                    );
                    *last_error = Some(format!(
                        "provider {} account {account_id}: refresh after token_expired failed",
                        provider.name
                    ));
                    AuthStep::Next
                }
            }
        }
        AuthErr401::Expired => {
            // The single retry for this attempt was already spent.
            debug!(
                "token_expired retry already consumed request_id={} provider={} account={account_id}",
                ctx.req_id, provider.name
            );
            AuthStep::FallThrough
        }
        AuthErr401::Revoked => {
            warn!(
                "upstream revoked credentials request_id={} provider={} account={account_id}, marking account revoked",
                ctx.req_id, provider.name
            );
            if let Err(err) = state.accounts.mark_revoked(account_id, text).await {
                warn!(
                    "failed to persist revoked marker request_id={} account={account_id} error={err}",
                    ctx.req_id
                );
            }
            *last_error = Some(format!(
                "provider {} account {account_id} revoked",
                provider.name
            ));
            if !has_next {
                return Err(revoked_account_response(account_id));
            }
            AuthStep::Next
        }
        AuthErr401::Unknown => {
            // No account semantics apply; with another target left (P9)
            // the next account/provider takes the request, otherwise the
            // upstream's own answer goes back unchanged.
            if has_next {
                AuthStep::Next
            } else {
                AuthStep::FallThrough
            }
        }
    };

    match step {
        AuthStep::RetrySame => {
            *expired_retried = Some(attempt);
            Ok(AuthStep::RetrySame)
        }
        AuthStep::Next => Ok(AuthStep::Next),
        AuthStep::FallThrough => Ok(AuthStep::FallThrough),
    }
}

/// Split a fully-buffered error [`Response`] into its body text plus a
/// rebuilt response, so the failover loop can classify 401 bodies while
/// still returning the original response unchanged when nothing applies.
async fn take_error_body(response: Response) -> (Response, String) {
    let (parts, body) = response.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => axum::body::Bytes::new(),
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (Response::from_parts(parts, Body::from(bytes)), text)
}

/// The unique provider's account was just revoked: the client must sign in
/// again, so say so instead of a generic gateway error.
fn revoked_account_response(account_id: &str) -> Response {
    api_error_response(
        StatusCode::BAD_GATEWAY,
        "account_revoked",
        format!(
            "account {account_id} was revoked by the upstream provider — sign in again to continue (`any-converter accounts add`)"
        ),
    )
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504 | 529)
}
