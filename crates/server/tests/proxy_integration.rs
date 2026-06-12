#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use any_converter_server::config::{ProviderConfig, RouteStrategy, ServerConfig, ServerSettings};
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Router, routing::post};
use reqwest::Client;
use serde_json::json;
use tower::ServiceExt;

fn oai_provider(name: &str, base_url: &str) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format: any_converter_core::convert::Format::OpenAIChat,
        base_url: base_url.into(),
        api_key: "upstream-key".into(),
        model_map: [("*".into(), "gpt-4".into())].into(),
    }
}

#[tokio::test]
async fn non_streaming_proxy() {
    // Upstream returns a valid chat completion response
    let upstream = Router::new()
        .route("/v1/chat/completions", post(|| async {
            axum::Json(json!({"id":"ok","object":"chat.completion","model":"gpt-4",
                "choices":[{"index":0,"message":{"role":"assistant","content":"Hello!"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":10,"completion_tokens":1,"total_tokens":11}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener, upstream).await.unwrap() });

    let base = format!("http://{addr}");
    let client = Client::builder().no_proxy().build().unwrap();
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
        },
        providers: vec![oai_provider("openai", &base)],
        model_routes: vec![],
        routes: vec![any_converter_server::config::RouteConfig {
            client_format: any_converter_core::convert::Format::OpenAIChat,
            provider: "openai".into(),
        }],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    let state = Arc::new(AppState {
        config,
        client,
        usage_logger: None,
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(val["choices"][0]["message"]["content"], "Hello!");
}

#[tokio::test]
async fn streaming_proxy_text() {
    use axum::response::sse::{Event, Sse};
    use futures_util::stream;
    use std::convert::Infallible;

    let upstream = Router::new()
        .route("/v1/chat/completions", post(|| async {
            Sse::new(stream::iter(vec![
                Ok::<Event, Infallible>(Event::default().data(r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#)),
                Ok::<Event, Infallible>(Event::default().data(r#"{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#)),
                Ok::<Event, Infallible>(Event::default().data("[DONE]")),
            ]))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener, upstream).await.unwrap() });

    let base = format!("http://{addr}");
    let client = Client::builder().no_proxy().build().unwrap();
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
        },
        providers: vec![oai_provider("openai", &base)],
        model_routes: vec![],
        routes: vec![any_converter_server::config::RouteConfig {
            client_format: any_converter_core::convert::Format::OpenAIChat,
            provider: "openai".into(),
        }],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    let state = Arc::new(AppState {
        config,
        client,
        usage_logger: None,
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains("Hello"),
        "expected Hello in streaming body: {body_str}"
    );
}

#[tokio::test]
async fn provider_failover_priority() {
    // Good upstream
    let good = Router::new()
        .route("/v1/chat/completions", post(|| async {
            axum::Json(json!({"id":"ok","object":"chat.completion","model":"gpt-4",
                "choices":[{"index":0,"message":{"role":"assistant","content":"Good!"},"finish_reason":"stop"}]}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let good_addr = listener.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener, good).await.unwrap() });

    let providers = vec![
        oai_provider("bad", "http://127.0.0.1:19999"), // nonexistent -> connection error
        oai_provider("good", &format!("http://{good_addr}")),
    ];
    let client = Client::builder().no_proxy().build().unwrap();
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
        },
        providers,
        model_routes: vec![any_converter_server::config::ModelRouteConfig {
            pattern: "*".into(),
            provider: None,
            providers: vec!["bad".into(), "good".into()],
            upstream_model: None,
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    let state = Arc::new(AppState {
        config,
        client,
        usage_logger: None,
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(val["choices"][0]["message"]["content"], "Good!");
}

#[tokio::test]
async fn auth_missing_returns_401() {
    let client = Client::new();
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: Some("secret".into()),
        },
        providers: vec![oai_provider("openai", "http://127.0.0.1:1")],
        model_routes: vec![],
        routes: vec![any_converter_server::config::RouteConfig {
            client_format: any_converter_core::convert::Format::OpenAIChat,
            provider: "openai".into(),
        }],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    let state = Arc::new(AppState {
        config,
        client,
        usage_logger: None,
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_invalid_returns_401() {
    let client = Client::new();
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: Some("secret".into()),
        },
        providers: vec![oai_provider("openai", "http://127.0.0.1:1")],
        model_routes: vec![],
        routes: vec![any_converter_server::config::RouteConfig {
            client_format: any_converter_core::convert::Format::OpenAIChat,
            provider: "openai".into(),
        }],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    let state = Arc::new(AppState {
        config,
        client,
        usage_logger: None,
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer wrong-key")
        .body(Body::from(
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn upstream_error_returns_502() {
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "upstream failure") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener, upstream).await.unwrap() });

    let base = format!("http://{addr}");
    let client = Client::builder().no_proxy().build().unwrap();
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
        },
        providers: vec![oai_provider("openai", &base)],
        model_routes: vec![],
        routes: vec![any_converter_server::config::RouteConfig {
            client_format: any_converter_core::convert::Format::OpenAIChat,
            provider: "openai".into(),
        }],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    let state = Arc::new(AppState {
        config,
        client,
        usage_logger: None,
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert!(response.status().is_server_error());
}

#[tokio::test]
async fn health_returns_ok() {
    let client = Client::new();
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
        },
        providers: vec![],
        model_routes: vec![],
        routes: vec![],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    let state = Arc::new(AppState {
        config,
        client,
        usage_logger: None,
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
