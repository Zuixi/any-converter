//! Accounts: what is stored, its allowance, signing in, and removing.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use log::warn;
use serde_json::Value;

use crate::auth;
use crate::credentials::allowance::copilot_allowance;
use crate::credentials::signout::{self, RevokeOutcome};
use crate::credentials::{Allowance, Credential, Limit};
use crate::signin_task::SignInTasks;

use super::{AppState, api_error_response, auth_error_response};

pub async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Quota share past which an account counts as used up (P9; magpie's
/// switch share, percent of the window — magpie's `Used` is 0–100 too).
pub(crate) const QUOTA_SHARE: f64 = 95.0;

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
pub(super) fn account_proxy(state: &AppState, provider: &str) -> Option<String> {
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

    // A provider a plugin serves is named `plugin:<id>`, and the request
    // says which package serves it (`spec`) and which of its methods to use.
    let spec = body.get("spec").and_then(|v| v.as_str());
    let method = body
        .get("method")
        .and_then(|v| v.as_u64())
        .map(|m| m as usize);
    let proxy = account_proxy(&state, provider);
    match sign_ins
        .start_with_proxy_at(provider, spec, method, proxy, state.accounts.clone())
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
pub(crate) fn cached_account_allowance(
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

pub(crate) async fn read_account_allowance(
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

/// `GET /v1/accounts/plugins` (P17): every installed plugin and what it
/// serves, so the window can offer a plugin's provider beside the built-in
/// ones. A spec that won't load is listed with why.
pub async fn handle_account_plugins(State(state): State<Arc<AppState>>) -> Response {
    let dir = crate::plugins::managed::plugins_dir();
    let installed = state.hosts.installed(&dir).await;
    let plugins: Vec<Value> = installed
        .into_iter()
        .map(|plugin| {
            let providers: Vec<Value> = plugin
                .providers
                .iter()
                .map(|provider| {
                    serde_json::json!({
                        "id": provider.id,
                        "name": provider.name,
                        "methods": provider.methods.iter().map(|method| serde_json::json!({
                            "type": match method.kind {
                                crate::plugins::AuthMethodKind::Oauth => "oauth",
                                crate::plugins::AuthMethodKind::Api => "api",
                            },
                            "label": method.label,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            serde_json::json!({
                "spec": plugin.spec,
                "providers": providers,
                "error": plugin.error,
            })
        })
        .collect();
    Json(serde_json::json!({
        "dir": dir.to_string_lossy(),
        "plugins": plugins,
    }))
    .into_response()
}

/// `POST /v1/accounts/order` {provider, accounts:[id...]} (P17): the order
/// routing tries an account provider's seats in. Fully-qualified account
/// ids, saved to `<config>/account-order.json` beside the secrets file —
/// the provider config stays the user's to edit.
pub async fn handle_account_order(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(err) = auth::validate_client_key(state.config.server.api_key.as_deref(), auth_header)
    {
        return auth_error_response(err);
    }
    let Some(provider) = body.get("provider").and_then(|v| v.as_str()) else {
        return api_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "provider is required",
        );
    };
    let Some(ids) = body.get("accounts").and_then(|v| v.as_array()) else {
        return api_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "accounts is required",
        );
    };
    let ids: Vec<String> = ids
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    match crate::provider_pool::save_order(&crate::config::secrets_path(), provider, &ids) {
        Ok(()) => Json(serde_json::json!({ "order": ids })).into_response(),
        Err(err) => api_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", err),
    }
}

/// `POST /v1/accounts/{id}/adopt` (P17): write this Codex account's rotated
/// tokens back to the `auth.json` it came from. `POST .../release` stops.
pub async fn handle_account_adopt(
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
    let secrets = crate::config::secrets_path();
    // Every Codex home this machine has, and the account each one holds:
    // an adopted account is written back where Codex itself reads it.
    let Ok(home) = std::env::var("HOME") else {
        return api_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_request_error",
            "HOME is not set",
        );
    };
    let homes = crate::credentials::codex_homes(
        &|key| std::env::var(key).ok(),
        std::path::Path::new(&home),
    );
    let found = homes.into_iter().find(|dir| {
        crate::credentials::read_codex(dir)
            .iter()
            .any(|a| a.account == id.trim_start_matches("codex:"))
    });
    let Some(dir) = found else {
        return api_error_response(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            format!("no Codex sign-in for {id}"),
        );
    };
    match crate::credentials::mark_adopted(&secrets, &id, &dir) {
        Ok(()) => Json(serde_json::json!({
            "adopted": id,
            "home": dir.to_string_lossy(),
        }))
        .into_response(),
        Err(err) => api_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", err),
    }
}

/// Stop writing tokens back for an adopted account.
pub async fn handle_account_release(
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
    match crate::credentials::release_adopted(&crate::config::secrets_path(), &id) {
        Ok(released) => Json(serde_json::json!({ "released": released })).into_response(),
        Err(err) => api_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", err),
    }
}
