//! HTTP surface. Shared state and the response helpers live here; each
//! concern has its own module: [`chat`] serves model requests, [`accounts`]
//! the account and sign-in endpoints, [`models`] the model list.

pub mod accounts;
pub mod chat;
pub mod models;
pub mod redact;
pub mod respond;
pub mod stream;

pub use accounts::{
    handle_account_adopt, handle_account_order, handle_account_plugins, handle_account_quotas,
    handle_account_release, handle_account_remove, handle_account_signin,
    handle_account_signin_cancel, handle_account_signin_status, handle_account_signin_submit,
    handle_accounts, handle_health,
};
pub use chat::{
    handle_claude, handle_count_tokens, handle_gemini, handle_openai_chat, handle_responses,
};
pub use models::{handle_model_retrieve, handle_models};

// What the sibling modules reach through `use super::*`. The first block is
// this family's own shared surface (the error shape, the redacting helpers,
// the streaming question); the second is the import list every module here
// needs, re-exported once instead of repeated in each file.
pub(crate) use redact::{sanitize_headers_for_log, truncated_body};
pub(crate) use respond::{
    HandlerError, api_error_response, auth_error_response, conversion_error_response,
    safe_convert_request, safe_convert_response, upstream_error_response,
};
pub(crate) use stream::is_streaming_request;

pub(crate) use std::collections::HashMap;
pub(crate) use std::sync::Arc;

pub(crate) use any_converter_core::convert::{Format, convert_request, convert_response};
pub(crate) use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
pub(crate) use log::{error, info, warn};
pub(crate) use reqwest::Client;
pub(crate) use serde_json::Value;

pub(crate) use crate::auth::{self, AuthError};
pub(crate) use crate::config::{RouteStrategy, ServerConfig, UpstreamApi};
pub(crate) use crate::credentials::allowance::CopilotAllowances;
pub(crate) use crate::credentials::copilot::CopilotSessions;
pub(crate) use crate::credentials::{AccountResolver, Allowance, ApiKeySource, TokenSource};
pub(crate) use crate::model::{extract_model_from_body, patch_model_in_body};
pub(crate) use crate::observability::utf8_prefix;
pub(crate) use crate::plugins::ManagedHosts;
pub(crate) use crate::provider_pool::order_accounts;
pub(crate) use crate::request_log::{RequestLogContext, RequestLogger};
pub(crate) use crate::route_strategy::order_provider_names;
pub(crate) use crate::router::RouteInfo;
pub(crate) use crate::usage::UsageLogger;

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

#[cfg(test)]
mod mod_test;
