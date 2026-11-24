//! HTTP surface. Shared state and the response helpers live here; each
//! concern has its own module: [`chat`] serves model requests, [`accounts`]
//! the account and sign-in endpoints, [`models`] the model list.

pub mod accounts;
pub mod chat;
pub mod data;
pub mod models;
pub mod redact;
pub mod respond;
pub mod stream;
pub mod usage;

pub use accounts::{
    handle_account_adopt, handle_account_models, handle_account_order, handle_account_plugins,
    handle_account_quotas, handle_account_release, handle_account_remove, handle_account_signin,
    handle_account_signin_cancel, handle_account_signin_status, handle_account_signin_submit,
    handle_accounts, handle_health, handle_plugin_install, handle_plugin_uninstall,
};
pub use chat::{
    handle_claude, handle_count_tokens, handle_gemini, handle_openai_chat, handle_responses,
};
pub use data::{
    handle_admin_convert, handle_admin_logs, handle_admin_session_transcript,
    handle_admin_sessions, handle_admin_status,
};
pub use models::{handle_model_retrieve, handle_models};
pub use usage::{handle_admin_usage, handle_usage_api};

// The family's own shared surface, reached by the sibling modules through
// `use super::...` / `use super::super::...`. Nothing else is re-exported:
// every module writes the imports it uses, so its dependencies are visible
// where it is read.
pub(crate) use redact::{sanitize_headers_for_log, truncated_body};
pub(crate) use respond::{
    HandlerError, api_error_response, auth_error_response, conversion_error_response,
    data_error_response, safe_convert_request, safe_convert_response, upstream_error_response,
};
pub(crate) use stream::is_streaming_request;

use std::sync::{Arc, RwLock};

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

/// The configuration the process is actually serving.
///
/// The control plane validates a patch and writes it to disk; the admin
/// handler then swaps this snapshot. Every read path — inference routing,
/// `/v1/models`, and agent wiring — goes through here, so none of them can
/// disagree with what was just saved. Resources built once at start-up
/// (the upstream client and its proxy, loggers, plugin hosts, the agent
/// registry's gateway address) keep their original settings and still need
/// a restart to change.
#[derive(Clone)]
pub struct LiveConfig(Arc<RwLock<Arc<ServerConfig>>>);

impl LiveConfig {
    pub fn new(config: ServerConfig) -> Self {
        Self(Arc::new(RwLock::new(Arc::new(config))))
    }

    /// The current snapshot; cloning one `Arc` per request.
    pub fn get(&self) -> Arc<ServerConfig> {
        Arc::clone(
            &self
                .0
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Replace the served snapshot after a validated config write.
    pub fn set(&self, config: ServerConfig) {
        *self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(config);
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: LiveConfig,
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
