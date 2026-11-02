#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings,
};
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
        account: None,
        accounts: Vec::new(),
        api: None,
        wire: None,
        model_map: [("*".into(), "gpt-4".into())].into(),
        endpoints: Default::default(),
        auth: Default::default(),
        plugin: None,
        proxy: None,
    }
}

fn make_config(providers: Vec<ProviderConfig>) -> ServerConfig {
    let names: Vec<String> = providers.iter().map(|p| p.name.clone()).collect();
    ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        },
        providers,
        model_routes: vec![ModelRouteConfig {
            pattern: "*".into(),
            provider: None,
            providers: names,
            upstream_model: None,
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: Default::default(),
        logging: Default::default(),
    }
}

fn make_app(config: ServerConfig) -> axum::Router {
    let state = Arc::new(AppState {
        config,
        client: Client::builder().no_proxy().build().unwrap(),
        accounts: any_converter_server::credentials::AccountResolver::with_default_store(
            Client::new(),
        ),
        usage_logger: None,
        request_logger: None,
        copilot: Arc::new(
            any_converter_server::credentials::copilot::CopilotSessions::new(Client::new()),
        ),
        hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
            Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                std::env::temp_dir().join("any-converter-test-plugin-auth.json"),
            )),
        )),
    });
    create_router_with_state(state)
}

async fn spawn_upstream(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

fn chat_request(streaming: bool) -> Request<Body> {
    let body = if streaming {
        r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"stream":true}"#
    } else {
        r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#
    };
    Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn rate_limited_upstream(hits: Arc<AtomicUsize>) -> Router {
    Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(json!({
                        "error": {
                            "message": "rate limited",
                            "type": "rate_limit_error",
                            "code": "rate_limit_exceeded"
                        }
                    })),
                )
            }
        }),
    )
}

fn ok_upstream(hits: Arc<AtomicUsize>) -> Router {
    Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "id": "b1",
                    "object": "chat.completion",
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "from-b"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}
                }))
            }
        }),
    )
}

fn streaming_ok_upstream(hits: Arc<AtomicUsize>) -> Router {
    use axum::response::sse::{Event, Sse};
    use futures_util::stream;
    use std::convert::Infallible;

    Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Sse::new(stream::iter(vec![
                    Ok::<Event, Infallible>(Event::default().data(
                        r#"{"id":"b1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"from-b"},"finish_reason":null}]}"#,
                    )),
                    Ok::<Event, Infallible>(Event::default().data(
                        r#"{"id":"b1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
                    )),
                    Ok::<Event, Infallible>(Event::default().data("[DONE]")),
                ]))
            }
        }),
    )
}

#[tokio::test]
async fn failover_retries_second_provider_after_429() {
    let hits_a = Arc::new(AtomicUsize::new(0));
    let hits_b = Arc::new(AtomicUsize::new(0));

    let base_a = spawn_upstream(rate_limited_upstream(hits_a.clone())).await;
    let base_b = spawn_upstream(ok_upstream(hits_b.clone())).await;

    let app = make_app(make_config(vec![
        oai_provider("a", &base_a),
        oai_provider("b", &base_b),
    ]));

    let response = app.oneshot(chat_request(false)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(val["choices"][0]["message"]["content"], "from-b");
    assert_eq!(hits_a.load(Ordering::SeqCst), 1);
    assert_eq!(hits_b.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn streaming_failover_after_429() {
    let hits_a = Arc::new(AtomicUsize::new(0));
    let hits_b = Arc::new(AtomicUsize::new(0));

    let base_a = spawn_upstream(rate_limited_upstream(hits_a.clone())).await;
    let base_b = spawn_upstream(streaming_ok_upstream(hits_b.clone())).await;

    let app = make_app(make_config(vec![
        oai_provider("a", &base_a),
        oai_provider("b", &base_b),
    ]));

    let response = app.oneshot(chat_request(true)).await.unwrap();
    // A retryable status on a streaming request fails over to the next
    // provider, exactly as the non-streaming path does (process_request's
    // shared is_retryable_status rule).
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("from-b"),
        "stream should serve from-b, got: {text}"
    );
    assert!(
        text.contains("[DONE]"),
        "stream should end with [DONE], got: {text}"
    );
    assert_eq!(hits_a.load(Ordering::SeqCst), 1);
    assert_eq!(hits_b.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn all_providers_429_returns_error() {
    let hits_a = Arc::new(AtomicUsize::new(0));
    let hits_b = Arc::new(AtomicUsize::new(0));

    let base_a = spawn_upstream(rate_limited_upstream(hits_a.clone())).await;
    let base_b = spawn_upstream(rate_limited_upstream(hits_b.clone())).await;

    let app = make_app(make_config(vec![
        oai_provider("a", &base_a),
        oai_provider("b", &base_b),
    ]));

    let response = app.oneshot(chat_request(false)).await.unwrap();
    assert!(
        response.status().as_u16() >= 400,
        "expected error status, got {}",
        response.status()
    );
    assert_eq!(hits_a.load(Ordering::SeqCst), 1);
    assert_eq!(hits_b.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn streaming_all_providers_429_passes_last_error_through() {
    let hits_a = Arc::new(AtomicUsize::new(0));
    let hits_b = Arc::new(AtomicUsize::new(0));

    let base_a = spawn_upstream(rate_limited_upstream(hits_a.clone())).await;
    let base_b = spawn_upstream(rate_limited_upstream(hits_b.clone())).await;

    let app = make_app(make_config(vec![
        oai_provider("a", &base_a),
        oai_provider("b", &base_b),
    ]));

    let response = app.oneshot(chat_request(true)).await.unwrap();
    // both providers are retryable-429 and exhausted: the last upstream
    // answer reaches the client instead of a synthetic gateway error
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hits_a.load(Ordering::SeqCst), 1);
    assert_eq!(hits_b.load(Ordering::SeqCst), 1);
}
