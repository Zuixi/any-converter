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

// ── Concurrent health checks ──────────────────────────────────────────

#[tokio::test]
async fn concurrent_health_checks_200() {
    let app = create_router(load_config(BASE_CONFIG_TOML));
    let mut handles = Vec::new();

    for _ in 0..200 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri("/health")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["status"], "ok");
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

// ── Concurrent model list requests ────────────────────────────────────

#[tokio::test]
async fn concurrent_models_requests() {
    let app = create_router(load_config(BASE_CONFIG_TOML));
    let mut handles = Vec::new();

    for _ in 0..100 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri("/v1/models")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["object"], "list");
            assert!(json["data"].is_array());
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

// ── Concurrent 404 requests ───────────────────────────────────────────

#[tokio::test]
async fn concurrent_404_requests() {
    let app = create_router(load_config(BASE_CONFIG_TOML));
    let mut handles = Vec::new();

    for i in 0..100 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri(format!("/nonexistent/{i}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

// ── Concurrent auth rejection ─────────────────────────────────────────

#[tokio::test]
async fn concurrent_auth_rejection() {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 8080
api_key = "secret"

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
    let mut handles = Vec::new();

    for _ in 0..100 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

// ── Mixed concurrent endpoints ────────────────────────────────────────

#[tokio::test]
async fn mixed_concurrent_endpoints() {
    let app = create_router(load_config(BASE_CONFIG_TOML));
    let mut handles = Vec::new();

    for i in 0..150 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let (uri, expected_status) = match i % 3 {
                0 => ("/health", StatusCode::OK),
                1 => ("/v1/models", StatusCode::OK),
                _ => ("/nonexistent", StatusCode::NOT_FOUND),
            };
            let resp = app
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                expected_status,
                "request {i} to {uri}: wrong status"
            );
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}
