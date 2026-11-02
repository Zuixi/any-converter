#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings,
};
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state;
use axum::body::Body;
use axum::extract::Request as AxumRequest;
use axum::http::{Request, StatusCode};
use axum::{Router, routing::post};
use reqwest::Client;
use serde_json::json;
use tokio::sync::Mutex;
use tower::ServiceExt;

const BODY: &str =
    r#"{"model":"claude-3","messages":[{"role":"user","content":"hello there, count me"}]}"#;

fn provider(name: &str, format: Format, base_url: &str) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format,
        base_url: base_url.into(),
        api_key: "upstream-key".into(),
        account: None,
        api: None,
        wire: None,
        model_map: [("*".into(), "claude-3-upstream".into())].into(),
        endpoints: Default::default(),
        auth: Default::default(),
    }
}

fn make_app(providers: Vec<ProviderConfig>, route_provider: &str, api_key: Option<&str>) -> Router {
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: api_key.map(String::from),
            pass_through_headers: PassThroughHeadersConfig::default(),
        },
        providers,
        model_routes: vec![ModelRouteConfig {
            pattern: "*".into(),
            provider: Some(route_provider.into()),
            providers: vec![],
            upstream_model: None,
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    create_router_with_state(Arc::new(AppState {
        config,
        client: Client::builder().no_proxy().build().unwrap(),
        accounts: any_converter_server::credentials::AccountResolver::with_default_store(
            Client::new(),
        ),
        usage_logger: None,
        request_logger: None,
    }))
}

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn count_tokens(
    app: Router,
    path: &str,
    auth: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .uri(path)
        .method("POST")
        .header("Content-Type", "application/json");
    if let Some(auth) = auth {
        builder = builder.header("Authorization", auth);
    }
    let response = app
        .oneshot(builder.body(Body::from(BODY)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

#[tokio::test]
async fn count_tokens_forwards_to_claude_upstream() {
    let captured_path = Arc::new(Mutex::new(String::new()));
    let captured_body = Arc::new(Mutex::new(serde_json::Value::Null));

    let upstream = Router::new().route(
        "/v1/messages/count_tokens",
        post({
            let path = captured_path.clone();
            let body = captured_body.clone();
            move |req: AxumRequest| async move {
                let seen_path = req.uri().path().to_string();
                let bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
                    .await
                    .unwrap();
                *path.lock().await = seen_path;
                *body.lock().await = serde_json::from_slice(&bytes).unwrap();
                axum::Json(json!({"input_tokens": 42}))
            }
        }),
    );
    let base = spawn(upstream).await;

    let app = make_app(
        vec![provider("claude", Format::Claude, &base)],
        "claude",
        None,
    );
    let (status, value) = count_tokens(app, "/v1/messages/count_tokens", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["input_tokens"], 42);
    assert_eq!(
        captured_path.lock().await.as_str(),
        "/v1/messages/count_tokens"
    );
    assert_eq!(captured_body.lock().await["model"], "claude-3-upstream");
}

#[tokio::test]
async fn count_tokens_upstream_error_falls_back_to_estimate() {
    let upstream = Router::new().route(
        "/v1/messages/count_tokens",
        post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
    );
    let base = spawn(upstream).await;

    let app = make_app(
        vec![provider("claude", Format::Claude, &base)],
        "claude",
        None,
    );
    let (status, value) = count_tokens(app, "/v1/messages/count_tokens", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(value["input_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn count_tokens_non_claude_provider_estimates_locally() {
    let hits = Arc::new(AtomicUsize::new(0));
    let upstream = Router::new().route(
        "/v1/messages/count_tokens",
        post({
            let hits = hits.clone();
            move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"input_tokens": 99}))
                }
            }
        }),
    );
    let base = spawn(upstream).await;

    let app = make_app(
        vec![provider("openai", Format::OpenAIChat, &base)],
        "openai",
        None,
    );
    let (status, value) = count_tokens(app, "/messages/count_tokens", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(value["input_tokens"].as_u64().unwrap() > 0);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn count_tokens_requires_auth() {
    let app = make_app(
        vec![provider("claude", Format::Claude, "http://127.0.0.1:1")],
        "claude",
        Some("secret"),
    );
    let (status, _) = count_tokens(app, "/v1/messages/count_tokens", None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
