use std::collections::HashMap;
use std::sync::Arc;

use any_converter_core::convert::{Format, convert_request, convert_response};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt;
use log::{debug, error, info, warn};
use reqwest::Client;
use serde_json::Value;

use crate::agent_id::agent_of;
use crate::auth::{self, AuthError};
use crate::config::{PluginRef, RouteStrategy, ServerConfig, UpstreamApi};
use crate::credentials::allowance::{CopilotAllowances, copilot_allowance};
use crate::credentials::copilot::{
    COPILOT_DEFAULT_API_BASE, CopilotError, CopilotSessions, copilot_api_headers,
};
use crate::credentials::signout::{self, RevokeOutcome};
use crate::credentials::token::{AuthErr401, classify_401};
use crate::credentials::{
    AccountResolver, Allowance, ApiKeySource, Credential, Limit, TokenSource,
};
use crate::model::{extract_model_from_body, patch_model_in_body, strip_private_fields};
use crate::namespace::{extract_namespace_map, patch_response_namespaces};
use crate::observability::utf8_prefix;
use crate::plugins::{LoadParams, ManagedHosts, PluginFetchParams, ProviderPlugin};
use crate::provider_pool::order_accounts;
use crate::proxy::build_upstream_url_for_provider;
use crate::request_log::{RequestLogContext, RequestLogger};
use crate::route_strategy::order_provider_names;
use crate::router::RouteInfo;
use crate::signin_task::SignInTasks;
use crate::usage::{self, UsageLogger, UsageRecord};

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub client: Client,
    /// First-class OAuth accounts (e.g. Gemini Code Assist sign-ins).
    pub accounts: Arc<AccountResolver>,
    /// Copilot session cache (GitHub OAuth token → short-lived Copilot
    /// session token) for `copilot:` account providers (P7).
    pub copilot: Arc<CopilotSessions>,
    /// Copilot quota windows, cached so routing never waits on one (P9).
    pub copilot_allowances: Arc<CopilotAllowances>,
    pub usage_logger: Option<Arc<UsageLogger>>,
    pub request_logger: Option<Arc<RequestLogger>>,
    /// Plugin subprocess hosts for providers with a `plugin` entry (P6.2).
    pub hosts: Arc<ManagedHosts>,
}

pub async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

pub async fn handle_models(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let mut available = state.config.available_models();
    // Plugin providers list their models at runtime (P6.2): fold the ids
    // into the catalog. A plugin that cannot answer only loses its entries.
    for provider in &state.config.providers {
        let Some(plugin_ref) = &provider.plugin else {
            continue;
        };
        let account = provider.account.clone().unwrap_or_default();
        match state.hosts.models_for(plugin_ref, &account).await {
            Ok(models) => {
                for model in models {
                    if !available.contains(&model.id) {
                        available.push(model.id);
                    }
                }
            }
            Err(err) => {
                warn!(
                    "plugin models failed provider={} error={err}, omitting plugin models",
                    provider.name
                );
            }
        }
    }

    // Code Assist accounts offer their models dynamically (P8): ask the
    // account the way Gemini CLI does and fold the result in after the
    // static catalog. `list_models` itself falls back to the full static
    // table when Code Assist can't be asked; the Err arm only guards
    // against a future strictness change and keeps the static catalog.
    for provider in &state.config.providers {
        if provider.effective_api() != UpstreamApi::CodeAssist {
            continue;
        }
        let Some(account_id) = provider.account.as_deref() else {
            continue;
        };
        let base = if provider.base_url.is_empty() {
            "https://cloudcode-pa.googleapis.com".to_string()
        } else {
            provider.base_url.clone()
        };
        match state.accounts.list_models(account_id, &base).await {
            Ok(models) => {
                for (id, _name) in models {
                    if !available.contains(&id) {
                        available.push(id);
                    }
                }
            }
            Err(err) => {
                warn!(
                    "code assist models unavailable provider={} error={err}, keeping the static catalog",
                    provider.name
                );
            }
        }
    }

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

/// Quota share past which an account counts as used up (P9; magpie's
/// switch share, percent of the window — magpie's `Used` is 0–100 too).
const QUOTA_SHARE: f64 = 95.0;

/// `GET /v1/accounts/quotas` (P9): every configured account's allowance
/// windows, one entry per account — accounts with no quota information
/// report `"windows": []`, never null.
pub async fn handle_account_quotas(State(state): State<Arc<AppState>>) -> Response {
    // The user asked for these, so they are read for real — but side by
    // side, so a pool of N accounts costs one round trip rather than N.
    let mut wanted = Vec::new();
    for provider in &state.config.providers {
        for account_id in provider.account_ids() {
            wanted.push((provider.clone(), account_id.to_string()));
        }
    }
    let reads = wanted.into_iter().map(|(provider, account_id)| {
        let state = Arc::clone(&state);
        async move {
            let allowance = read_account_allowance(&state, &provider, &account_id).await;
            let windows: Vec<serde_json::Value> = allowance
                .map(|allowance| allowance.iter().map(quota_window_json).collect())
                .unwrap_or_default();
            serde_json::json!({
                "provider": account_id.split(':').next().unwrap_or(""),
                "account": account_id,
                "windows": windows,
            })
        }
    });
    let accounts: Vec<serde_json::Value> = futures_util::future::join_all(reads).await;
    Json(serde_json::json!({ "accounts": accounts })).into_response()
}

/// One allowance window as the quotas endpoint reports it.
fn quota_window_json(window: &Limit) -> serde_json::Value {
    serde_json::json!({
        "name": window.model.as_deref().unwrap_or("all"),
        "used": window.used,
        "resets": window.resets.map(|at| {
            chrono::DateTime::<chrono::Utc>::from(at)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }),
    })
}

/// `GET /v1/accounts` (P10): the locally stored account secrets, one entry
/// per id with `{id, kind, provider, account}` — **never** the tokens.
///
/// Kept public like [`handle_account_quotas`]: the entries carry no
/// secret material.
pub async fn handle_accounts(State(state): State<Arc<AppState>>) -> Response {
    let credentials = match state.accounts.list_credentials() {
        Ok(credentials) => credentials,
        Err(err) => {
            return api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                format!("reading the account store failed: {err}"),
            );
        }
    };

    let accounts: Vec<serde_json::Value> = credentials
        .into_iter()
        .map(|(id, credential)| {
            let (kind, provider, account) = match &credential {
                Credential::OAuth {
                    provider, account, ..
                } => ("oauth", provider.clone(), account.clone()),
                Credential::ApiKey { .. } => (
                    "api_key",
                    id.split(':').next().unwrap_or("").to_string(),
                    String::new(),
                ),
            };
            serde_json::json!({ "id": id, "kind": kind, "provider": provider, "account": account })
        })
        .collect();
    Json(serde_json::json!({ "accounts": accounts })).into_response()
}

/// The egress proxy for one provider's account traffic: the provider's own
/// `proxy` field, else the server-level one, else none (P10).
fn account_proxy(state: &AppState, provider: &str) -> Option<String> {
    let by_account = state.config.providers.iter().find(|p| {
        p.account_ids()
            .iter()
            .any(|id| id.split(':').next() == Some(provider))
    });
    let provider_config = by_account.or_else(|| state.config.find_provider(provider));
    provider_config
        .and_then(|p| p.proxy_for(state.config.server.proxy.as_deref()))
        .map(String::from)
        .or_else(|| state.config.server.proxy.clone())
}

/// `POST /v1/accounts/signin` (P10): start a gemini/copilot sign-in task
/// and return its snapshot at once.
pub async fn handle_account_signin(
    State(state): State<Arc<AppState>>,
    Extension(sign_ins): Extension<Arc<SignInTasks>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(err) = auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
    {
        return auth_error_response(err);
    }

    let provider = body.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    if provider.is_empty() {
        return api_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "missing provider (gemini or copilot)",
        );
    }
    // `account` is accepted for magpie parity but unused: the account id
    // is discovered from the flow result (gemini email / copilot hint).

    let proxy = account_proxy(&state, provider);
    match sign_ins
        .start_with_proxy(provider, proxy, state.accounts.clone())
        .await
    {
        Ok(task) => Json(task).into_response(),
        Err(err) => api_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", err),
    }
}

/// `GET /v1/accounts/signin/{id}` (P10): poll one sign-in task.
pub async fn handle_account_signin_status(
    State(state): State<Arc<AppState>>,
    Extension(sign_ins): Extension<Arc<SignInTasks>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(err) = auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
    {
        return auth_error_response(err);
    }

    match sign_ins.status(&id) {
        Some(task) => Json(task).into_response(),
        None => api_error_response(
            StatusCode::NOT_FOUND,
            "signin_task_not_found",
            format!("sign-in task {id} not found"),
        ),
    }
}

/// `POST /v1/accounts/signin/{id}/submit` (P10): finish a flow with a
/// pasted callback URL (gemini).
pub async fn handle_account_signin_submit(
    State(state): State<Arc<AppState>>,
    Extension(sign_ins): Extension<Arc<SignInTasks>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(err) = auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
    {
        return auth_error_response(err);
    }

    let input = body.get("input").and_then(|v| v.as_str()).unwrap_or("");
    match sign_ins.submit(&id, input).await {
        Ok(task) => Json(task).into_response(),
        Err(err) => api_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", err),
    }
}

/// `POST /v1/accounts/signin/{id}/cancel` (P10): cancel one sign-in task
/// (closing its listener / poller); 204 when it existed.
pub async fn handle_account_signin_cancel(
    State(state): State<Arc<AppState>>,
    Extension(sign_ins): Extension<Arc<SignInTasks>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(err) = auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
    {
        return auth_error_response(err);
    }

    if sign_ins.cancel(&id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        api_error_response(
            StatusCode::NOT_FOUND,
            "signin_task_not_found",
            format!("sign-in task {id} not found"),
        )
    }
}

/// `POST /v1/accounts/{id}/remove` (P10): revoke the vendor token when the
/// vendor offers a revocation endpoint, then remove the account locally.
/// The response reports the revocation outcome (`Revoked` | `NotOffered`
/// | `Failed`); a failed revocation never blocks the removal.
pub async fn handle_account_remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(err) = auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
    {
        return auth_error_response(err);
    }

    let provider = id.split(':').next().unwrap_or("");
    let proxy = account_proxy(&state, provider);
    match signout::remove(&state.accounts, &id, proxy.as_deref()).await {
        Ok(outcome) => {
            let (name, error) = match &outcome {
                RevokeOutcome::Revoked => ("Revoked", None),
                RevokeOutcome::NotOffered => ("NotOffered", None),
                RevokeOutcome::Failed(err) => ("Failed", Some(err.clone())),
            };
            Json(serde_json::json!({ "id": id, "outcome": name, "error": error })).into_response()
        }
        Err(err) => api_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_error",
            format!("removing account {id} failed: {err}"),
        ),
    }
}

/// The allowance windows of one account of `provider` (P9). Gemini and
/// Antigravity report them through `fetchAvailableModels` (the resolver
/// caches the reading); Copilot reports one window through the Copilot
/// user endpoint; every other vendor says nothing → `None`.
/// The account's allowance as last read, **without any I/O**, kicking off a
/// background refresh when it is missing or stale.
///
/// The request path uses this: a quota read must never delay a request
/// (magpie reads allowances in the background). `read_account_allowance`
/// stays for the quotas endpoint and the UI, where the user is asking.
fn cached_account_allowance(
    state: &AppState,
    provider: &crate::config::ProviderConfig,
    account_id: &str,
) -> Option<Allowance> {
    if account_id.split(':').next() == Some("copilot") {
        return state.copilot_allowances.fresh(account_id).or_else(|| {
            state.copilot_allowances.refresh_in_background(
                Arc::clone(&state.accounts),
                Arc::clone(&state.copilot),
                state.client.clone(),
                account_id.to_string(),
            );
            None
        });
    }
    if let Some(allowance) = state.accounts.allowance_fresh(account_id) {
        return Some(allowance);
    }
    let base = if provider.base_url.is_empty() {
        "https://cloudcode-pa.googleapis.com"
    } else {
        provider.base_url.as_str()
    };
    state
        .accounts
        .refresh_allowance_in_background(account_id, base);
    None
}

async fn read_account_allowance(
    state: &AppState,
    provider: &crate::config::ProviderConfig,
    account_id: &str,
) -> Option<Allowance> {
    if account_id.split(':').next() == Some("copilot") {
        let github = match state.accounts.resolve(account_id).await {
            Ok(account) => account,
            Err(err) => {
                warn!("allowance read skipped account={account_id} error={err}");
                return None;
            }
        };
        let session = match state.copilot.session(&github.access_token).await {
            Ok(session) => session,
            Err(err) => {
                warn!("allowance read skipped account={account_id} error={err}");
                return None;
            }
        };
        return copilot_allowance(&state.client, &session).await;
    }
    let base = if provider.base_url.is_empty() {
        "https://cloudcode-pa.googleapis.com"
    } else {
        provider.base_url.as_str()
    };
    state.accounts.allowance(account_id, base).await
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
fn estimate_input_tokens(body: &Value) -> u64 {
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
/// The user-agent is normalized to a known agent id (see [`crate::agent_id`]).
fn extract_client_id(headers: &HeaderMap) -> Option<String> {
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
            UpstreamApi::CodeAssist => crate::wire::build_url(
                UpstreamApi::CodeAssist,
                &provider.base_url,
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

        let ids = provider.account_ids();
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

/// Outcome of one plugin-transport attempt in the failover loop.
enum PluginOutcome {
    /// The plugin answered; the shared retryable-status rule applies.
    Answered(Response, StatusCode),
    /// The plugin transport failed; move to the next provider.
    Failed { error: String },
}

/// Send the already-converted request through the plugin's `fetch` hook.
///
/// URL construction mirrors magpie `pluginFetch`: the loader's base URL
/// (trailing slash trimmed, falling back to the matched model's own `url`)
/// plus [`UpstreamApi::path`]. A same-host guard keeps the plugin from
/// steering credentialed requests anywhere but its own base host.
#[allow(clippy::too_many_arguments)] // provider-branch boundary: state, config, and converted request parts
async fn forward_via_plugin(
    state: &AppState,
    plugin_ref: &PluginRef,
    provider: &crate::config::ProviderConfig,
    api: UpstreamApi,
    upstream_headers: Vec<(String, String)>,
    converted_body: Vec<u8>,
    ctx: &mut RequestContext,
    route_info: RouteInfo,
) -> PluginOutcome {
    let account = provider.account.clone().unwrap_or_default();

    // Lazily start (and reuse) the plugin subprocess; `host_for` restarts
    // a dead process once.
    let host = match state.hosts.host_for(plugin_ref).await {
        Ok(host) => host,
        Err(err) => {
            error!("plugin start failed provider={} error={err}", provider.name);
            return PluginOutcome::Failed {
                error: format!("plugin {}: {err}", provider.name),
            };
        }
    };
    let provider_id = host.info().id.clone();

    // The loader decides where and how the request is sent. The host never
    // injects a local Authorization header for plugin providers — the
    // plugin signs its own requests.
    let loader = match ProviderPlugin::load(
        &*host,
        LoadParams {
            provider: provider_id.clone(),
            account: account.clone(),
            proxy: None,
        },
    )
    .await
    {
        Ok(options) => options,
        Err(err) => {
            error!("plugin load failed provider={} error={err}", provider.name);
            return PluginOutcome::Failed {
                error: format!("plugin {} load: {err}", provider.name),
            };
        }
    };

    // Model list: `npm` travels in the fetch params and identifies the wire
    // the body was shaped as (`resolve_upstream_api` used the same list).
    let models = match state.hosts.models_for(plugin_ref, &account).await {
        Ok(models) => models,
        Err(err) => {
            error!(
                "plugin models failed provider={} error={err}",
                provider.name
            );
            return PluginOutcome::Failed {
                error: format!("plugin {} models: {err}", provider.name),
            };
        }
    };
    let matched = models.iter().find(|m| {
        m.id == ctx.upstream_model || (!m.api_id.is_empty() && m.api_id == ctx.upstream_model)
    });

    // base = loader.base_url (trailing slash trimmed), else the model's own
    // `url`; an empty base means the plugin cannot send requests at all.
    let mut base = loader.base_url.trim_end_matches('/').to_string();
    if base.is_empty() {
        base = matched
            .map(|m| m.url.trim_end_matches('/').to_string())
            .unwrap_or_default();
    }
    if base.is_empty() {
        error!(
            "plugin provider={} has nowhere to send its requests",
            provider.name
        );
        return PluginOutcome::Answered(
            api_error_response(
                StatusCode::BAD_GATEWAY,
                "plugin_error",
                format!(
                    "plugin says nowhere to send its requests (provider {})",
                    provider.name
                ),
            ),
            StatusCode::BAD_GATEWAY,
        );
    }

    // The AI SDK's base ends where magpie's `/v1` does: magpie sends
    // `base + TrimPrefix(rest, "/v1")` (plugins.go pluginFetch), so a
    // loader base of https://api.githubcopilot.com asks for
    // /chat/completions, and one of https://api.anthropic.com/v1 must not
    // become /v1/v1/messages.
    let path = api.path(&ctx.upstream_model, ctx.streaming);
    let path = path.strip_prefix("/v1").unwrap_or(&path);
    let url = format!("{base}{path}");
    // Same-host guard (magpie pluginFetch): credentialed requests must stay
    // on the loader's host; relative/hostless bases fail it outright.
    if !crate::proxy::assert_same_host(&base, &url) {
        error!(
            "plugin provider={} tried to send a request outside its base host",
            provider.name
        );
        return PluginOutcome::Answered(
            api_error_response(
                StatusCode::BAD_GATEWAY,
                "plugin_error",
                "plugin tried to send a request to a different host than its base URL",
            ),
            StatusCode::BAD_GATEWAY,
        );
    }

    if let Some(ref mut log_ctx) = ctx.log_ctx {
        log_ctx.upstream_url = url.clone();
    }

    let params = PluginFetchParams {
        url,
        method: "POST".to_string(),
        headers: upstream_headers,
        body: String::from_utf8_lossy(&converted_body).into_owned(),
        provider: provider_id,
        account,
        model: ctx.upstream_model.clone(),
        npm: matched.map(|m| m.npm.clone()).unwrap_or_default(),
    };

    let from_format = api.to_format();
    let to_format = route_info.client_format;
    if ctx.streaming {
        match crate::proxy::forward_plugin_streaming(
            &*host,
            params,
            from_format,
            to_format,
            api,
            &ctx.ns_map,
        )
        .await
        {
            Ok(pair) => PluginOutcome::Answered(pair.0, pair.1),
            Err(err) => PluginOutcome::Failed { error: err },
        }
    } else {
        match crate::proxy::forward_plugin_non_streaming(
            &*host,
            params,
            from_format,
            to_format,
            api,
            &ctx.ns_map,
        )
        .await
        {
            Ok(pair) => PluginOutcome::Answered(pair.0, pair.1),
            Err(err) => PluginOutcome::Failed { error: err },
        }
    }
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
    Envelope(String),
    Panic,
}

impl std::fmt::Display for HandlerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandlerError::Conversion(e) => write!(f, "{e}"),
            HandlerError::Envelope(e) => write!(f, "{e}"),
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
                    proxy: None,
                },
                providers: vec![ProviderConfig {
                    name: "openai".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://api.openai.com".into(),
                    api_key: "sk-test".into(),
                    account: None,
                    accounts: Vec::new(),
                    api: None,
                    wire: None,
                    model_map: [("*".into(), "gpt-4.1".into())].into(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                    plugin: None,
                    proxy: None,
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
            accounts: AccountResolver::with_default_store(Client::new()),
            usage_logger: None,
            request_logger: None,
            copilot: Arc::new(CopilotSessions::new(Client::new())),
            copilot_allowances: Arc::new(CopilotAllowances::default()),
            hosts: Arc::new(crate::plugins::ManagedHosts::with_store(Arc::new(
                crate::plugins::PluginAuthStore::new(
                    tempfile::tempdir().unwrap().path().join("auth.json"),
                ),
            ))),
        }
    }

    #[test]
    fn test_extract_client_id_normalizes_user_agent() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static("claude-cli/2.1.0 (external, cli)"),
        );
        assert_eq!(extract_client_id(&headers), Some("claude-code".into()));

        let mut headers = HeaderMap::new();
        headers.insert("x-requested-with", HeaderValue::from_static("MyApp"));
        assert_eq!(extract_client_id(&headers), Some("MyApp".into()));
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

    #[test]
    fn estimate_input_tokens_sums_nested_string_bytes() {
        let body = serde_json::json!({
            "model": "claude-3",
            "system": "abcd",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "abcdefgh"}]}],
            "tools": [{"name": "tool", "description": "desc", "parameters": {"k": "vvvv"}}],
        });
        // strings: claude-3(8) + abcd(4) + user(4) + text(4) + abcdefgh(8) + tool(4) + desc(4) + k(1) + vvvv(4) = 41
        assert_eq!(estimate_input_tokens(&body), 41 / 4);
    }

    #[test]
    fn estimate_input_tokens_of_empty_body_is_zero() {
        assert_eq!(estimate_input_tokens(&serde_json::json!({})), 0);
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
                    proxy: None,
                },
                providers: vec![],
                model_routes: vec![],
                routes: vec![],
                model_metadata: HashMap::new(),
                logging: LoggingConfig::default(),
            },
            client: Client::new(),
            accounts: AccountResolver::with_default_store(Client::new()),
            usage_logger: None,
            request_logger: None,
            copilot: Arc::new(CopilotSessions::new(Client::new())),
            copilot_allowances: Arc::new(CopilotAllowances::default()),
            hosts: Arc::new(crate::plugins::ManagedHosts::with_store(Arc::new(
                crate::plugins::PluginAuthStore::new(
                    tempfile::tempdir().unwrap().path().join("auth.json"),
                ),
            ))),
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
                .find_provider("openai")
                .unwrap()
                .resolve_model("claude-3"),
            "gpt-4.1"
        );
    }
}
