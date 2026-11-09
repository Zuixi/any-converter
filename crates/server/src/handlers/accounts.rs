//! Accounts: what is stored, its allowance, signing in, and removing.

use std::collections::HashSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use log::warn;
use serde_json::Value;

use crate::credentials::allowance::copilot_allowance;
use crate::credentials::copilot::{COPILOT_DEFAULT_API_BASE, copilot_api_headers};
use crate::credentials::signout::{self, RevokeOutcome};
use crate::credentials::siwc_signin::{ReqwestSiwc, SiwcEndpoints, SiwcSignIn};
use crate::credentials::{Allowance, Credential, Limit};
use crate::plugins::PluginAuth;
use crate::signin_task::SignInTasks;

use super::{AppState, api_error_response};

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

/// One account as the management API lists it. Plugin auth entries live in
/// a separate ledger, but they are accounts the user signed in just the
/// same; `source` lets the window tell which store owns one without ever
/// exposing its credential.
#[derive(Debug, Clone)]
struct ListedAccount {
    id: String,
    kind: &'static str,
    provider: String,
    account: String,
    source: &'static str,
}

impl ListedAccount {
    fn json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "kind": self.kind,
            "provider": self.provider,
            "account": self.account,
            "source": self.source,
        })
    }
}

/// One model row in the account-model response. Dynamic discovery and
/// configured aliases can both contribute, so the source travels with the
/// row instead of being flattened to one answer-wide label.
#[derive(Debug, Clone)]
struct AccountModel {
    id: String,
    name: String,
    source: String,
}

/// Every account the server can act on: the built-in secret store first,
/// then plugin OAuth/API-key entries. Internal plugin ledger rows (the
/// fixture uses `__gw_seen`) are not accounts. A duplicate id always
/// resolves to the built-in credential, matching request routing.
fn listed_accounts(state: &AppState) -> Result<Vec<ListedAccount>, String> {
    let credentials = state
        .accounts
        .list_credentials()
        .map_err(|err| format!("reading the account store failed: {err}"))?;

    let mut seen = HashSet::new();
    let mut accounts = Vec::with_capacity(credentials.len());
    for (id, credential) in credentials {
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
        seen.insert(id.clone());
        accounts.push(ListedAccount {
            id,
            kind,
            provider,
            account,
            source: "builtin",
        });
    }

    let plugin_auth = match state.hosts.auth_store().load() {
        Ok(auth) => auth,
        Err(err) => {
            warn!("reading the plugin auth store failed: {err}");
            return Ok(accounts);
        }
    };
    for (id, auth) in plugin_auth {
        if id.starts_with("__") || seen.contains(&id) {
            continue;
        }
        let Some((provider, account)) = id.split_once(':') else {
            continue;
        };
        if provider.is_empty() || account.is_empty() {
            continue;
        }
        let kind = match auth {
            PluginAuth::OAuth { .. } => "oauth",
            PluginAuth::ApiKey { .. } => "api_key",
        };
        seen.insert(id.clone());
        let provider = provider.to_string();
        let account = account.to_string();
        accounts.push(ListedAccount {
            id,
            kind,
            provider,
            account,
            source: "plugin",
        });
    }
    Ok(accounts)
}

/// `GET /v1/accounts` (P10): the locally stored accounts, one entry per id
/// with `{id, kind, provider, account, source}` — **never** the tokens.
///
/// Kept public like [`handle_account_quotas`]: the entries carry no
/// secret material.
pub async fn handle_accounts(State(state): State<Arc<AppState>>) -> Response {
    let accounts = match listed_accounts(&state) {
        Ok(accounts) => accounts,
        Err(err) => {
            return api_error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error", err);
        }
    };

    let accounts: Vec<Value> = accounts.iter().map(ListedAccount::json).collect();
    Json(serde_json::json!({ "accounts": accounts })).into_response()
}

/// The provider that actually names this account. This is the only signal
/// that requests for the account can be routed through a configured
/// provider; a provider merely called `chatgpt-api` does not bind it.
fn bound_provider<'a>(
    state: &'a AppState,
    account: &ListedAccount,
) -> Option<&'a crate::config::ProviderConfig> {
    state
        .config
        .providers
        .iter()
        .find(|provider| provider.account_ids().contains(&account.id.as_str()))
}

/// A provider whose `base_url` can inform discovery. Exact account binding
/// wins; the name fallback exists for unbound sign-ins (and test/fake
/// endpoints), but must never be reported as a binding.
fn provider_hint<'a>(
    state: &'a AppState,
    account: &ListedAccount,
) -> Option<&'a crate::config::ProviderConfig> {
    bound_provider(state, account).or_else(|| {
        state
            .config
            .providers
            .iter()
            .find(|provider| provider.name == account.provider || provider.name == account.id)
    })
}

/// Literal configured model names for one provider: `model_map` keys and
/// `model_routes` patterns. A glob describes a family, not a selectable
/// model id, so it is not shown as one.
fn configured_models(
    state: &AppState,
    provider: &crate::config::ProviderConfig,
) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for id in provider.model_map.keys() {
        if !id.contains('*') && !id.contains('?') && seen.insert(id.clone()) {
            models.push((id.clone(), id.clone()));
        }
    }
    for route in &state.config.model_routes {
        let named = route
            .provider_names()
            .iter()
            .any(|name| name == &provider.name);
        if named
            && !route.pattern.contains('*')
            && !route.pattern.contains('?')
            && seen.insert(route.pattern.clone())
        {
            models.push((route.pattern.clone(), route.pattern.clone()));
        }
    }
    models.sort_by(|left, right| left.0.cmp(&right.0));
    models
}

/// Ask OpenAI's own model list for a Sign in with ChatGPT account. The
/// provider, when present, only supplies a fake/alternate base URL; the
/// account itself need not be bound to a provider for discovery to work.
async fn discover_chatgpt_models(
    state: &AppState,
    account: &ListedAccount,
    provider: Option<&crate::config::ProviderConfig>,
) -> Result<Vec<(String, String)>, String> {
    let resolved = state
        .accounts
        .resolve(&account.id)
        .await
        .map_err(|err| err.to_string())?;
    let base = provider
        .map(crate::config::ProviderConfig::chatgpt_api_base)
        .unwrap_or_else(|| "https://api.openai.com".to_string());
    let endpoints = SiwcEndpoints {
        api: format!("{}/v1", base.trim_end_matches('/')),
        ..SiwcEndpoints::default()
    };
    let sign_in =
        SiwcSignIn::with_http(endpoints, Arc::new(ReqwestSiwc::new(state.client.clone())));
    let models = sign_in
        .models(&account.id, &resolved.access_token)
        .await
        .map_err(|err| err.to_string())?;
    Ok(models.into_iter().map(|id| (id.clone(), id)).collect())
}

/// Ask Copilot's own API for the models this session can use. The session
/// response carries the effective API base; the public default is used only
/// when GitHub omits it.
async fn discover_copilot_models(
    state: &AppState,
    account: &ListedAccount,
) -> Result<Vec<(String, String)>, String> {
    let github = state
        .accounts
        .resolve(&account.id)
        .await
        .map_err(|err| err.to_string())?;
    let session = state
        .copilot
        .session(&github.access_token)
        .await
        .map_err(|err| err.to_string())?;
    let base = if session.api_base.is_empty() {
        COPILOT_DEFAULT_API_BASE
    } else {
        session.api_base.as_str()
    };
    let url = format!("{}/models", base.trim_end_matches('/'));
    let mut request = state.client.get(url);
    for (name, value) in copilot_api_headers(&session.token) {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("copilot models request failed: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("copilot models returned {status}"));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|err| format!("copilot models returned malformed payload: {err}"))?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id").and_then(Value::as_str))
                .filter(|id| !id.is_empty())
                .map(|id| (id.to_string(), id.to_string()))
                .collect()
        })
        .unwrap_or_default())
}

/// Append discovered rows first, then configured aliases, keeping the first
/// occurrence of each model id. This lets a `"*"` route stay a wildcard while
/// the actual model list still comes from the account.
fn merge_models(
    models: &mut Vec<AccountModel>,
    seen: &mut HashSet<String>,
    found: Vec<(String, String)>,
    source: &str,
) {
    for (id, name) in found {
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        models.push(AccountModel {
            id,
            name,
            source: source.to_string(),
        });
    }
}

/// The plugin that serves an account: the configured provider that names
/// exactly this account first, else an installed package advertising the
/// account's provider id.
async fn plugin_for_account(
    state: &AppState,
    account: &ListedAccount,
) -> Option<crate::config::PluginRef> {
    if let Some(provider) = state.config.providers.iter().find(|provider| {
        provider.plugin.is_some() && provider.account_ids().contains(&account.id.as_str())
    }) {
        return provider.plugin.clone();
    }

    let dir = crate::plugins::managed::plugins_dir();
    let installed = state.hosts.installed(&dir).await;
    installed
        .into_iter()
        .find(|plugin| {
            plugin.error.is_empty()
                && plugin
                    .providers
                    .iter()
                    .any(|provider| provider.id == account.provider)
        })
        .map(|plugin| crate::config::PluginRef {
            spec: plugin.spec,
            target: dir.to_string_lossy().into_owned(),
        })
}

/// `GET /v1/accounts/{id}/models`: the models one account can actually
/// offer. Discovery failures are a successful answer carrying `error` while
/// retaining any literal configured models: the account still exists, and
/// the window needs the reason rather than a generic 500. An unknown account
/// is a 404.
pub async fn handle_account_models(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let accounts = match listed_accounts(&state) {
        Ok(accounts) => accounts,
        Err(err) => {
            return api_error_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error", err);
        }
    };
    let Some(account) = accounts.into_iter().find(|account| account.id == id) else {
        return api_error_response(
            StatusCode::NOT_FOUND,
            "account_not_found",
            format!("account {id} not found"),
        );
    };

    let bound = bound_provider(&state, &account);
    let hint = provider_hint(&state, &account);
    let mut source = "unsupported".to_string();
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    let mut error = None;

    if account.source == "plugin" {
        source = "plugin".to_string();
        match plugin_for_account(&state, &account).await {
            Some(plugin_ref) => match state.hosts.models_for(&plugin_ref, &account.account).await {
                Ok(found) => {
                    let found: Vec<(String, String)> = found
                        .into_iter()
                        .filter(|model| !model.id.is_empty())
                        .map(|model| {
                            let name = if model.name.is_empty() {
                                model.id.clone()
                            } else {
                                model.name
                            };
                            (model.id, name)
                        })
                        .collect();
                    merge_models(&mut models, &mut seen, found, &source);
                }
                Err(err) => {
                    warn!(
                        "plugin model discovery failed account={} error={err}",
                        account.id
                    );
                    error = Some(err.to_string());
                }
            },
            None => {
                error = Some(format!(
                    "no installed plugin serves provider `{}`",
                    account.provider
                ));
            }
        }
    } else if matches!(account.provider.as_str(), "gemini" | "antigravity") {
        source = if account.provider == "antigravity" {
            "antigravity"
        } else {
            "gemini"
        }
        .to_string();
        let base = hint
            .map(crate::config::ProviderConfig::code_assist_load_base)
            .unwrap_or_else(|| crate::config::GEMINI_CODE_ASSIST_BASE.to_string());
        match state.accounts.discover_models(&account.id, &base).await {
            Ok(found) => merge_models(&mut models, &mut seen, found, &source),
            Err(err) => {
                warn!(
                    "code assist model discovery failed account={} error={err}",
                    account.id
                );
                error = Some(err.to_string());
            }
        }
    } else if account.provider == "chatgpt-api" {
        source = "chatgpt-api".to_string();
        match discover_chatgpt_models(&state, &account, hint).await {
            Ok(found) => merge_models(&mut models, &mut seen, found, &source),
            Err(err) => {
                warn!(
                    "ChatGPT API model discovery failed account={} error={err}",
                    account.id
                );
                error = Some(err);
            }
        }
    } else if account.provider == "copilot" {
        source = "copilot".to_string();
        match discover_copilot_models(&state, &account).await {
            Ok(found) => merge_models(&mut models, &mut seen, found, &source),
            Err(err) => {
                warn!(
                    "Copilot model discovery failed account={} error={err}",
                    account.id
                );
                error = Some(err);
            }
        }
    }

    // A configured provider is also a useful fallback when discovery is not
    // available (Codex/Claude today) or failed. It is merged, not substituted:
    // an empty discovered list must not erase literal configured aliases.
    if let Some(provider) = bound {
        let configured = configured_models(&state, provider);
        if source == "unsupported" && !configured.is_empty() {
            source = "configured".to_string();
        }
        merge_models(&mut models, &mut seen, configured, "configured");
    }

    let models: Vec<Value> = models
        .into_iter()
        .map(|model| {
            serde_json::json!({
                "id": model.id,
                "name": model.name,
                "source": model.source,
            })
        })
        .collect();
    Json(serde_json::json!({
        "account": account.id,
        "provider": account.provider,
        "models": models,
        "source": source,
        "bound": bound.is_some(),
        "error": error,
    }))
    .into_response()
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
    Json(body): Json<serde_json::Value>,
) -> Response {
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
    Extension(sign_ins): Extension<Arc<SignInTasks>>,
    Path(id): Path<String>,
) -> Response {
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
    Extension(sign_ins): Extension<Arc<SignInTasks>>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let input = body.get("input").and_then(|v| v.as_str()).unwrap_or("");
    match sign_ins.submit(&id, input).await {
        Ok(task) => Json(task).into_response(),
        Err(err) => api_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", err),
    }
}

/// `POST /v1/accounts/signin/{id}/cancel` (P10): cancel one sign-in task
/// (closing its listener / poller); 204 when it existed.
pub async fn handle_account_signin_cancel(
    Extension(sign_ins): Extension<Arc<SignInTasks>>,
    Path(id): Path<String>,
) -> Response {
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
    Path(id): Path<String>,
) -> Response {
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
pub async fn handle_account_order(Json(body): Json<Value>) -> Response {
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
pub async fn handle_account_adopt(Path(id): Path<String>) -> Response {
    let secrets = crate::config::secrets_path();
    // Every Codex home this machine has, and the account each one holds:
    // an adopted account is written back where Codex itself reads it.
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
    let Ok(home) = home else {
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
pub async fn handle_account_release(Path(id): Path<String>) -> Response {
    match crate::credentials::release_adopted(&crate::config::secrets_path(), &id) {
        Ok(released) => Json(serde_json::json!({ "released": released })).into_response(),
        Err(err) => api_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", err),
    }
}
