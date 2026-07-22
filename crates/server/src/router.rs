use std::path::PathBuf;
use std::sync::Arc;

use any_converter_core::convert::Format;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use reqwest::Client;

use crate::config::ServerConfig;
use crate::handlers::{
    AppState, handle_claude, handle_gemini, handle_health, handle_model_retrieve, handle_models,
    handle_openai_chat, handle_responses,
};

/// Route metadata detected from the request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteInfo {
    pub client_format: Format,
    pub path_streaming: bool,
}

/// Detect client format from a request path.
///
/// Returns `None` for special endpoints like `/health` and `/v1/models`.
pub fn detect_format_from_path(path: &str) -> Option<RouteInfo> {
    match path {
        "/v1/chat/completions" | "/chat/completions" => Some(RouteInfo {
            client_format: Format::OpenAIChat,
            path_streaming: false,
        }),
        "/v1/messages" | "/messages" => Some(RouteInfo {
            client_format: Format::Claude,
            path_streaming: false,
        }),
        "/v1/responses" | "/responses" => Some(RouteInfo {
            client_format: Format::OpenAIResponses,
            path_streaming: false,
        }),
        path if path.starts_with("/v1beta/models/") => parse_gemini_path(path),
        _ => None,
    }
}

fn parse_gemini_path(path: &str) -> Option<RouteInfo> {
    let rest = path.strip_prefix("/v1beta/models/")?;
    if rest.ends_with(":streamGenerateContent") {
        Some(RouteInfo {
            client_format: Format::Gemini,
            path_streaming: true,
        })
    } else if rest.ends_with(":generateContent") {
        Some(RouteInfo {
            client_format: Format::Gemini,
            path_streaming: false,
        })
    } else {
        None
    }
}

/// Extract the model name from a Gemini path.
pub fn extract_gemini_model(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1beta/models/")?;
    if let Some(model) = rest.strip_suffix(":streamGenerateContent") {
        Some(model.to_string())
    } else {
        rest.strip_suffix(":generateContent")
            .map(|model| model.to_string())
    }
}

pub fn create_router(config: ServerConfig) -> Router {
    let log_dir = config.logging.dir.clone();
    let sqlite = crate::storage::open_sqlite_storage_for_log_dir(log_dir.as_deref());
    let usage_logger = log_dir.as_ref().map(|dir| {
        Arc::new(crate::usage::UsageLogger::with_sqlite(
            PathBuf::from(dir),
            sqlite.clone(),
        ))
    });
    let request_logger = if config.logging.request_log.enabled {
        log_dir.as_ref().map(|dir| {
            Arc::new(crate::request_log::RequestLogger::with_sqlite(
                PathBuf::from(dir),
                sqlite.clone(),
            ))
        })
    } else {
        None
    };

    if let Some(ref dir) = log_dir {
        let max_bytes = config.logging.max_disk_mb * 1024 * 1024;
        let _disk_quota =
            crate::disk_quota::spawn_disk_quota_manager(PathBuf::from(dir), max_bytes);
    }

    let state = Arc::new(AppState {
        config,
        client: Client::new(),
        usage_logger,
        request_logger,
    });
    create_router_with_state(state)
}

/// Create a router with a pre-built `AppState`, enabling test injection of
/// custom HTTP clients (e.g., pointing at a local `wiremock` server).
pub fn create_router_with_state(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/v1/models", get(handle_models))
        .route("/models", get(handle_models))
        .route("/v1/models/{model_id}", get(handle_model_retrieve))
        .route("/models/{model_id}", get(handle_model_retrieve))
        // With /v1/ prefix (standard)
        .route("/v1/chat/completions", post(handle_openai_chat))
        .route("/v1/messages", post(handle_claude))
        .route("/v1/responses", post(handle_responses))
        // Without /v1/ prefix (Codex CLI and some clients)
        .route("/chat/completions", post(handle_openai_chat))
        .route("/messages", post(handle_claude))
        .route("/responses", post(handle_responses))
        .route("/v1beta/models/{*model_action}", post(handle_gemini))
        .fallback(handle_not_found)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)) // 10 MB
        .with_state(state)
}

async fn handle_not_found(uri: axum::http::Uri) -> (axum::http::StatusCode, &'static str) {
    log::warn!("request to unknown path path={uri}");
    (axum::http::StatusCode::NOT_FOUND, "not found")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use crate::config::{
        LoggingConfig, PassThroughHeadersConfig, ProviderConfig, RouteConfig, ServerSettings,
    };

    fn test_config() -> ServerConfig {
        ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
            },
            providers: vec![ProviderConfig {
                name: "openai".into(),
                format: Format::OpenAIChat,
                base_url: "https://api.openai.com".into(),
                api_key: "sk-test".into(),
                model_map: [("claude-sonnet-4".into(), "gpt-4.1".into())].into(),
                endpoints: Default::default(),
                auth: Default::default(),
            }],
            model_routes: vec![],
            routes: vec![
                RouteConfig {
                    client_format: Format::OpenAIChat,
                    provider: "openai".into(),
                },
                RouteConfig {
                    client_format: Format::Claude,
                    provider: "openai".into(),
                },
            ],
            model_metadata: std::collections::HashMap::new(),
            logging: LoggingConfig::default(),
        }
    }

    #[test]
    fn test_detect_format_openai_chat() {
        let info = detect_format_from_path("/v1/chat/completions").unwrap();
        assert_eq!(info.client_format, Format::OpenAIChat);
        assert!(!info.path_streaming);
    }

    #[test]
    fn test_detect_format_claude() {
        let info = detect_format_from_path("/v1/messages").unwrap();
        assert_eq!(info.client_format, Format::Claude);
    }

    #[test]
    fn test_detect_format_responses() {
        let info = detect_format_from_path("/v1/responses").unwrap();
        assert_eq!(info.client_format, Format::OpenAIResponses);
    }

    #[test]
    fn test_detect_format_gemini_stream() {
        let info =
            detect_format_from_path("/v1beta/models/gemini-pro:streamGenerateContent").unwrap();
        assert_eq!(info.client_format, Format::Gemini);
        assert!(info.path_streaming);
    }

    #[test]
    fn test_detect_format_gemini_non_stream() {
        let info = detect_format_from_path("/v1beta/models/gemini-pro:generateContent").unwrap();
        assert_eq!(info.client_format, Format::Gemini);
        assert!(!info.path_streaming);
    }

    #[test]
    fn test_detect_format_unknown_path() {
        assert!(detect_format_from_path("/unknown").is_none());
        assert!(detect_format_from_path("/health").is_none());
    }

    #[tokio::test]
    async fn test_health_check_returns_200() {
        let app = create_router(test_config());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_unknown_path_returns_404() {
        let app = create_router(test_config());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_models_endpoint_returns_model_list() {
        let app = create_router(test_config());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["object"], "list");
        assert!(!json["data"].as_array().unwrap().is_empty());
    }
}
