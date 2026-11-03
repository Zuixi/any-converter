//! Structural route guard.
//!
//! axum cannot enumerate a `Router`'s routes at runtime, so this is an explicit
//! inventory: keep `ROUTES` in sync with `create_router_with_state`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings,
};
use any_converter_server::credentials::AccountResolver;
use any_converter_server::handlers::AppState;
use any_converter_server::router::{create_router_with_state, detect_format_from_path};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use reqwest::Client;
use tower::ServiceExt;

/// Every route registered by `create_router_with_state`.
///
/// `(method, path, minimal body)` — GET routes carry an empty body.
const ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/health", ""),
    ("GET", "/v1/models", ""),
    ("GET", "/models", ""),
    ("GET", "/v1/models/guard-model", ""),
    ("GET", "/models/guard-model", ""),
    ("GET", "/v1/accounts/quotas", ""),
    ("GET", "/v1/accounts/plugins", ""),
    (
        "POST",
        "/v1/accounts/order",
        r#"{"provider":"x","accounts":[]}"#,
    ),
    ("POST", "/v1/accounts/guard/remove", ""),
    // adopt/release are absent on purpose: with no Codex sign-in on the test
    // machine they answer 404 themselves, which is a handler verdict, not a
    // missing route. They are covered by accounts_extra_e2e instead.
    ("GET", "/v1/agents", ""),
    ("GET", "/v1/agents/claude-code/keys", ""),
    // Invalid body: reaches the handler (400) without touching a real home.
    ("POST", "/v1/agents/claude-code/connect", "not-json"),
    ("POST", "/v1/agents/claude-code/disconnect", ""),
    ("POST", "/v1/agents/claude-code/sync", ""),
    ("POST", "/v1/agents/claude-code/key/rotate", ""),
    (
        "POST",
        "/v1/chat/completions",
        r#"{"model":"guard-model","messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/chat/completions",
        r#"{"model":"guard-model","messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/v1/messages",
        r#"{"model":"guard-model","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/messages",
        r#"{"model":"guard-model","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/v1/responses",
        r#"{"model":"guard-model","input":"hi"}"#,
    ),
    (
        "POST",
        "/responses",
        r#"{"model":"guard-model","input":"hi"}"#,
    ),
    (
        "POST",
        "/v1/messages/count_tokens",
        r#"{"model":"guard-model","messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/messages/count_tokens",
        r#"{"model":"guard-model","messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/v1beta/models/m1:generateContent",
        r#"{"contents":[{"parts":[{"text":"hi"}]}]}"#,
    ),
];

fn make_app(base_url: &str, api_key: Option<&str>) -> Router {
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: api_key.map(String::from),
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        },
        providers: vec![ProviderConfig {
            name: "openai".into(),
            format: Format::OpenAIChat,
            base_url: base_url.into(),
            api_key: "upstream-key".into(),
            account: None,
            accounts: Vec::new(),
            api: None,
            wire: None,
            // Literal key so `/models/{model_id}` resolves instead of 404.
            model_map: [("guard-model".into(), "upstream-model".into())].into(),
            endpoints: Default::default(),
            auth: Default::default(),
            plugin: None,
            proxy: None,
        }],
        model_routes: vec![ModelRouteConfig {
            pattern: "*".into(),
            provider: Some("openai".into()),
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
        accounts: AccountResolver::with_default_store(Client::new()),
        copilot_allowances: Arc::new(
            any_converter_server::credentials::CopilotAllowances::default(),
        ),
        usage_logger: None,
        request_logger: None,
        copilot: Arc::new(
            any_converter_server::credentials::copilot::CopilotSessions::new(Client::new()),
        ),
        agents: Arc::new(any_converter_server::agents::AgentRegistry::in_memory()),
        hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
            Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                std::env::temp_dir().join("any-converter-test-plugin-auth.json"),
            )),
        )),
    }))
}

async fn spawn_upstream(hits: Arc<AtomicUsize>) -> String {
    let router = Router::new().fallback(move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            (StatusCode::INTERNAL_SERVER_ERROR, "upstream down")
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn send(app: Router, method: &str, path: &str, body: &str, auth: Option<&str>) -> StatusCode {
    let mut builder = Request::builder()
        .uri(path)
        .method(method)
        .header("Content-Type", "application/json");
    if let Some(auth) = auth {
        builder = builder.header("Authorization", auth);
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn every_route_answers_not_404() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let app = make_app(&base, None);

    for (method, path, body) in ROUTES {
        let status = send(app.clone(), method, path, body, None).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} fell through to the 404 fallback"
        );
    }
}

#[tokio::test]
async fn post_routes_require_auth_and_never_reach_upstream() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits.clone()).await;
    let app = make_app(&base, Some("secret"));

    for (method, path, body) in ROUTES {
        if *method != "POST" {
            continue;
        }
        let status = send(app.clone(), method, path, body, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} accepted a request without Authorization"
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "unauthenticated POST reached the upstream"
    );

    // Public GET endpoints stay reachable when an api_key is configured.
    for path in ["/health", "/v1/models"] {
        let status = send(app.clone(), "GET", path, "", None).await;
        assert_eq!(status, StatusCode::OK, "GET {path} should be public");
    }
}

#[test]
fn detect_format_matches_client_paths() {
    let expected = [
        ("/v1/chat/completions", Some(Format::OpenAIChat)),
        ("/v1/messages", Some(Format::Claude)),
        ("/v1/responses", Some(Format::OpenAIResponses)),
        ("/v1beta/models/x:generateContent", Some(Format::Gemini)),
        (
            "/v1beta/models/x:streamGenerateContent",
            Some(Format::Gemini),
        ),
        ("/health", None),
        ("/v1/models", None),
        ("/no/such/route", None),
    ];

    for (path, format) in expected {
        assert_eq!(
            detect_format_from_path(path).map(|info| info.client_format),
            format,
            "format drift for {path}"
        );
    }
}
