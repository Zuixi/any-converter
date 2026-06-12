#![allow(clippy::unwrap_used, clippy::expect_used)]

use any_converter_server::config::ServerConfig;
use any_converter_server::router::create_router;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

const BASE_CONFIG_TOML: &str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "test-key"

[[routes]]
client_format = "openai_chat"
provider = "openai"
"#;

fn load_config(toml: &str) -> ServerConfig {
    ServerConfig::from_toml(toml).expect("config should parse")
}

async fn send_request(app: axum::Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body.to_vec())
}

#[tokio::test]
async fn health_returns_ok() {
    let app = create_router(load_config(BASE_CONFIG_TOML));
    let (status, body) = send_request(
        app,
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn models_returns_model_list() {
    let app = create_router(load_config(BASE_CONFIG_TOML));
    let (status, body) = send_request(
        app,
        Request::builder()
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["object"], "list");
    assert!(json["data"].is_array());
}

#[tokio::test]
async fn nonexistent_path_returns_404() {
    let app = create_router(load_config(BASE_CONFIG_TOML));
    let (status, _) = send_request(
        app,
        Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn chat_completions_without_auth_returns_401_when_key_configured() {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 8080
api_key = "client-secret"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "test-key"

[[routes]]
client_format = "openai_chat"
provider = "openai"
"#;
    let app = create_router(load_config(toml));
    let body = r#"{"model":"gpt-4.1","messages":[{"role":"user","content":"hi"}]}"#;
    let (status, response_body) = send_request(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let json: serde_json::Value = serde_json::from_slice(&response_body).unwrap();
    assert_eq!(json["error"]["type"], "authentication_error");
}

#[tokio::test]
async fn models_with_client_version_returns_codex_format() {
    let app = create_router(load_config(BASE_CONFIG_TOML));
    let (status, body) = send_request(
        app,
        Request::builder()
            .uri("/v1/models?client_version=1.0")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("models").is_some());
    assert!(json.get("data").is_none());
    assert!(json["models"].is_array());
}
