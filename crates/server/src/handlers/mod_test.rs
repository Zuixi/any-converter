//! Tests for the shared handler state and the response helpers.
#![allow(clippy::unwrap_used)]

use super::chat::{estimate_input_tokens, extract_client_id, extract_session_id, process_request};
use super::*;
use crate::config::{
    LoggingConfig, PassThroughHeadersConfig, ProviderConfig, RouteConfig, ServerSettings,
};
use crate::router::RouteInfo;
use any_converter_core::convert::Format;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::{HeaderMap, header};
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
        agents: Arc::new(crate::agents::AgentRegistry::in_memory()),
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
        agents: Arc::new(crate::agents::AgentRegistry::in_memory()),
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
