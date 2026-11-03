//! HTTP surface. Shared state and the response helpers live here; each
//! concern has its own module: [`chat`] serves model requests, [`accounts`]
//! the account and sign-in endpoints, [`models`] the model list.

pub mod accounts;
pub mod chat;
pub mod models;

pub use accounts::{
    handle_account_quotas, handle_account_remove, handle_account_signin,
    handle_account_signin_cancel, handle_account_signin_status, handle_account_signin_submit,
    handle_accounts, handle_health,
};
pub use chat::{
    handle_claude, handle_count_tokens, handle_gemini, handle_openai_chat, handle_responses,
};
pub use models::{handle_model_retrieve, handle_models};

pub(crate) use std::collections::HashMap;
pub(crate) use std::sync::Arc;

pub(crate) use any_converter_core::convert::{Format, convert_request, convert_response};
pub(crate) use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
pub(crate) use http_body_util::BodyExt;
pub(crate) use log::{debug, error, info, warn};
pub(crate) use reqwest::Client;
pub(crate) use serde_json::Value;

pub(crate) use crate::agent_id::agent_of;
pub(crate) use crate::auth::{self, AuthError};
pub(crate) use crate::config::{PluginRef, RouteStrategy, ServerConfig, UpstreamApi};
pub(crate) use crate::credentials::allowance::{CopilotAllowances, copilot_allowance};
pub(crate) use crate::credentials::copilot::{
    COPILOT_DEFAULT_API_BASE, CopilotError, CopilotSessions, copilot_api_headers,
};
pub(crate) use crate::credentials::signout::{self, RevokeOutcome};
pub(crate) use crate::credentials::token::{AuthErr401, classify_401};
pub(crate) use crate::credentials::{
    AccountResolver, Allowance, ApiKeySource, Credential, Limit, TokenSource,
};
pub(crate) use crate::model::{extract_model_from_body, patch_model_in_body, strip_private_fields};
pub(crate) use crate::namespace::{extract_namespace_map, patch_response_namespaces};
pub(crate) use crate::observability::utf8_prefix;
pub(crate) use crate::plugins::{LoadParams, ManagedHosts, PluginFetchParams, ProviderPlugin};
pub(crate) use crate::provider_pool::order_accounts;
pub(crate) use crate::proxy::build_upstream_url_for_provider;
pub(crate) use crate::request_log::{RequestLogContext, RequestLogger};
pub(crate) use crate::route_strategy::order_provider_names;
pub(crate) use crate::router::RouteInfo;
pub(crate) use crate::signin_task::SignInTasks;
pub(crate) use crate::usage::{self, UsageLogger, UsageRecord};

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

    use super::chat::{
        estimate_input_tokens, extract_client_id, extract_session_id, process_request,
    };
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
