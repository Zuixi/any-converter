//! HTTP surface. Shared state and the response helpers live here; each
//! concern has its own module: [`chat`] serves model requests, [`accounts`]
//! the account and sign-in endpoints, [`models`] the model list.

pub mod accounts;
pub mod chat;
pub mod models;
pub mod redact;
pub mod respond;
pub mod stream;
pub mod usage;

pub use accounts::{
    handle_account_adopt, handle_account_models, handle_account_order, handle_account_plugins,
    handle_account_quotas, handle_account_release, handle_account_remove, handle_account_signin,
    handle_account_signin_cancel, handle_account_signin_status, handle_account_signin_submit,
    handle_accounts, handle_health,
};
pub use chat::{
    handle_claude, handle_count_tokens, handle_gemini, handle_openai_chat, handle_responses,
};
pub use models::{handle_model_retrieve, handle_models};
pub use usage::handle_usage_api;

// The family's own shared surface, reached by the sibling modules through
// `use super::...` / `use super::super::...`. Nothing else is re-exported:
// every module writes the imports it uses, so its dependencies are visible
// where it is read.
pub(crate) use redact::{sanitize_headers_for_log, truncated_body};
pub(crate) use respond::{
    HandlerError, api_error_response, auth_error_response, conversion_error_response,
    safe_convert_request, safe_convert_response, upstream_error_response,
};
pub(crate) use stream::is_streaming_request;

use std::sync::Arc;

use crate::agents::AgentRegistry;
use crate::config::ServerConfig;
use crate::credentials::AccountResolver;
use crate::credentials::allowance::CopilotAllowances;
use crate::credentials::copilot::CopilotSessions;
use crate::plugins::ManagedHosts;
use crate::request_log::RequestLogger;
use crate::usage::UsageLogger;
use reqwest::Client;

// Nothing is re-exported for the sibling modules: each of them writes its
// own imports, so a module's dependencies are visible where it is read.

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
    /// Agent wiring registry: per-agent gateway keys and wiring state.
    pub agents: Arc<AgentRegistry>,
}

#[cfg(test)]
mod mod_test;
