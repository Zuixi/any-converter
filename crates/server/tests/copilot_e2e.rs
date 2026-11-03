//! End-to-end tests for the GitHub Copilot wire (P7): the GitHub OAuth
//! token → Copilot session exchange (a `GET` authenticated with `token`,
//! per magpie `copilotToken`), the per-model `/chat/completions` vs
//! `/responses` dispatch, session caching, near-expiry re-exchange, and the
//! refusal surface — all against fake GitHub and Copilot API endpoints.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    LoggingConfig, ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy,
    ServerConfig, ServerSettings,
};
use any_converter_server::credentials::copilot::CopilotSessions;
use any_converter_server::credentials::{
    AccountResolver, Credential, FileSecretStore, SecretStore,
};
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state;
use axum::body::Body;
use axum::extract::Request as AxumRequest;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    Router,
    extract::State as AxumState,
    routing::{get, post},
};
use reqwest::Client;
use serde_json::{Value, json};
use tower::ServiceExt;

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ── Fake GitHub (token exchange) + fake Copilot API ──────────────────────

/// One scripted token-exchange answer. When the script queue is empty the
/// exchange defaults to a fresh, long-lived session.
enum ExchangeScript {
    /// 200 with the given session token and expiry.
    Session { token: String, expires_at: i64 },
    /// 401 (stale GitHub token).
    Refused,
}

/// Everything the fake upstreams observed, shared with the test.
struct FakeUpstream {
    exchange_calls: Mutex<usize>,
    exchange_scripts: Mutex<VecDeque<ExchangeScript>>,
    last_exchange_method: Mutex<Option<String>>,
    last_exchange_auth: Mutex<Option<String>>,
    /// The fake service's own base URL, embedded into `endpoints.api`.
    api_base: Mutex<String>,
    api_calls: Mutex<usize>,
    api_path: Mutex<Option<String>>,
    api_auth: Mutex<Option<String>>,
    api_headers: Mutex<Vec<(String, String)>>,
    api_body: Mutex<Option<Value>>,
}

impl Default for FakeUpstream {
    fn default() -> Self {
        Self {
            exchange_calls: Mutex::new(0),
            exchange_scripts: Mutex::new(VecDeque::new()),
            last_exchange_method: Mutex::new(None),
            last_exchange_auth: Mutex::new(None),
            api_base: Mutex::new(String::new()),
            api_calls: Mutex::new(0),
            api_path: Mutex::new(None),
            api_auth: Mutex::new(None),
            api_headers: Mutex::new(Vec::new()),
            api_body: Mutex::new(None),
        }
    }
}

impl FakeUpstream {
    fn set_api_base(&self, base: &str) {
        *self.api_base.lock().unwrap() = base.to_string();
    }

    /// Queue one successful exchange answer.
    fn push_session(&self, token: &str, expires_at: i64) {
        self.exchange_scripts
            .lock()
            .unwrap()
            .push_back(ExchangeScript::Session {
                token: token.to_string(),
                expires_at,
            });
    }

    /// Queue one refused (401) exchange answer.
    fn push_refused(&self) {
        self.exchange_scripts
            .lock()
            .unwrap()
            .push_back(ExchangeScript::Refused);
    }

    fn exchange_calls(&self) -> usize {
        *self.exchange_calls.lock().unwrap()
    }

    fn last_exchange_method(&self) -> Option<String> {
        self.last_exchange_method.lock().unwrap().clone()
    }

    fn last_exchange_auth(&self) -> Option<String> {
        self.last_exchange_auth.lock().unwrap().clone()
    }

    fn api_calls(&self) -> usize {
        *self.api_calls.lock().unwrap()
    }

    fn api_path(&self) -> Option<String> {
        self.api_path.lock().unwrap().clone()
    }

    fn api_auth(&self) -> Option<String> {
        self.api_auth.lock().unwrap().clone()
    }

    fn api_header(&self, name: &str) -> Option<String> {
        self.api_headers
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    }

    fn api_body(&self) -> Option<Value> {
        self.api_body.lock().unwrap().clone()
    }
}

/// The GitHub token exchange (magpie copilotToken): a `GET` answering
/// `{token, expires_at, endpoints: {api}}`.
async fn token_exchange(
    AxumState(state): AxumState<Arc<FakeUpstream>>,
    req: AxumRequest,
) -> Response {
    *state.exchange_calls.lock().unwrap() += 1;
    *state.last_exchange_method.lock().unwrap() = Some(req.method().as_str().to_string());
    *state.last_exchange_auth.lock().unwrap() = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let script = state.exchange_scripts.lock().unwrap().pop_front();
    match script.unwrap_or(ExchangeScript::Session {
        token: "cpt_session_default".to_string(),
        expires_at: unix_now() + 3600,
    }) {
        ExchangeScript::Session { token, expires_at } => axum::Json(json!({
            "token": token,
            "expires_at": expires_at,
            "endpoints": { "api": state.api_base.lock().unwrap().as_str() },
        }))
        .into_response(),
        ExchangeScript::Refused => (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"message": "bad github token"})),
        )
            .into_response(),
    }
}

/// One Copilot API endpoint (mounted on both `/chat/completions` and
/// `/responses`): record the request, answer in the matching wire shape.
async fn api_endpoint(
    AxumState(state): AxumState<Arc<FakeUpstream>>,
    req: AxumRequest,
) -> Response {
    *state.api_calls.lock().unwrap() += 1;
    *state.api_path.lock().unwrap() = Some(req.uri().path().to_string());
    *state.api_auth.lock().unwrap() = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    *state.api_headers.lock().unwrap() = [
        "copilot-integration-id",
        "x-github-api-version",
        "editor-version",
        "editor-plugin-version",
        "user-agent",
    ]
    .iter()
    .filter_map(|name| {
        req.headers()
            .get(*name)
            .and_then(|v| v.to_str().ok())
            .map(|v| (name.to_string(), v.to_string()))
    })
    .collect();
    let body = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .unwrap();
    *state.api_body.lock().unwrap() = serde_json::from_slice(&body).ok().or(Some(Value::Null));

    if state.api_path.lock().unwrap().as_deref() == Some("/responses") {
        axum::Json(json!({
            "id": "resp_1",
            "object": "response",
            "created_at": unix_now(),
            "model": "gpt-5.6-luna",
            "status": "completed",
            "output": [
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "hello copilot"}
                ]}
            ],
            "usage": {"input_tokens": 5, "output_tokens": 2}
        }))
        .into_response()
    } else {
        axum::Json(json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": unix_now(),
            "model": "claude-sonnet-4-5",
            "choices": [{"index": 0, "message": {
                "role": "assistant",
                "content": "hello copilot"
            }, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
        }))
        .into_response()
    }
}

/// Spawn the fake GitHub + Copilot API service on an ephemeral port;
/// returns its base URL (used as `endpoints.api`).
async fn spawn_fakes() -> (String, Arc<FakeUpstream>) {
    let state = Arc::new(FakeUpstream::default());
    let router = Router::new()
        .route("/copilot_internal/v2/token", get(token_exchange))
        .route("/chat/completions", post(api_endpoint))
        .route("/responses", post(api_endpoint))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    state.set_api_base(&base);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, state)
}

// ── Test scaffolding ──────────────────────────────────────────────────────

/// The `copilot:octocat` GitHub OAuth credential stored in the account store.
fn copilot_credential() -> Credential {
    Credential::OAuth {
        provider: "copilot".into(),
        account: "octocat".into(),
        access_token: "gho_github_token".into(),
        refresh_token: None,
        expires_at_unix: None,
        siwc: None,
    }
}

/// Config with one `copilot:` account provider whose `base_url` is empty —
/// the session's `endpoints.api` is the only source of the upstream URL.
fn server_config() -> ServerConfig {
    ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        },
        providers: vec![ProviderConfig {
            name: "copilot".into(),
            format: Format::OpenAIChat,
            base_url: String::new(),
            api_key: String::new(),
            account: Some("copilot:octocat".into()),
            accounts: Vec::new(),
            api: None,
            wire: None,
            model_map: Default::default(),
            endpoints: Default::default(),
            auth: Default::default(),
            plugin: None,
            proxy: None,
        }],
        model_routes: vec![ModelRouteConfig {
            pattern: "*".into(),
            provider: Some("copilot".into()),
            providers: vec![],
            upstream_model: None,
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: Default::default(),
        logging: LoggingConfig::default(),
    }
}

/// A temp secrets store plus the router wired over the fake services.
struct TestEnv {
    _dir: tempfile::TempDir,
    app: Router,
    upstream: Arc<FakeUpstream>,
}

impl TestEnv {
    async fn build() -> Self {
        let (base, upstream) = spawn_fakes().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let store = FileSecretStore::new(secrets_path);
        store.put("copilot:octocat", &copilot_credential()).unwrap();

        let client = Client::builder().no_proxy().build().unwrap();
        let accounts = Arc::new(AccountResolver::new(store, client.clone()));
        let copilot = Arc::new(
            CopilotSessions::new(client.clone())
                .with_token_url(&format!("{base}/copilot_internal/v2/token")),
        );
        let state = Arc::new(AppState {
            config: server_config(),
            client,
            accounts,
            copilot,
            copilot_allowances: Arc::new(
                any_converter_server::credentials::CopilotAllowances::default(),
            ),
            usage_logger: None,
            request_logger: None,
            hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
                Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                    std::env::temp_dir().join("any-converter-test-plugin-auth.json"),
                )),
            )),
        });
        let app = create_router_with_state(state);

        Self {
            _dir: dir,
            app,
            upstream,
        }
    }

    async fn chat(&self, model: &str) -> (StatusCode, String) {
        chat_via(&self.app, model).await
    }
}

/// Send one `/v1/chat/completions` request for `model` and return
/// (status, body).
async fn chat_via(app: &Router, model: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": model,
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn copilot_exchange_then_chat() {
    let env = TestEnv::build().await;

    let (status, body) = env.chat("claude-sonnet-4-5").await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["choices"][0]["message"]["content"], "hello copilot");

    // The exchange happened exactly once, as a GET authenticated with the
    // literal `token <github>` prefix (not Bearer)...
    assert_eq!(env.upstream.exchange_calls(), 1);
    assert_eq!(env.upstream.last_exchange_method().as_deref(), Some("GET"));
    assert_eq!(
        env.upstream.last_exchange_auth().as_deref(),
        Some("token gho_github_token")
    );

    // ...and the API request carried the session as Bearer plus the
    // Copilot client headers and the API version, on /chat/completions
    // (the `/v1` prefix stripped, base taken from `endpoints.api`).
    assert_eq!(env.upstream.api_calls(), 1);
    assert_eq!(
        env.upstream.api_path().as_deref(),
        Some("/chat/completions")
    );
    assert_eq!(
        env.upstream.api_auth().as_deref(),
        Some("Bearer cpt_session_default")
    );
    assert_eq!(
        env.upstream.api_header("copilot-integration-id").as_deref(),
        Some("vscode-chat")
    );
    assert_eq!(
        env.upstream.api_header("x-github-api-version").as_deref(),
        Some("2026-01-09")
    );
    assert_eq!(
        env.upstream.api_header("editor-version").as_deref(),
        Some("vscode/1.140.0")
    );
}

#[tokio::test]
async fn copilot_gpt5_goes_to_responses() {
    let env = TestEnv::build().await;

    let (status, body) = env.chat("gpt-5.6-luna").await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["choices"][0]["message"]["content"], "hello copilot");

    // gpt-5 and up (except gpt-5-mini) answer on /responses.
    assert_eq!(env.upstream.api_path().as_deref(), Some("/responses"));
    assert_eq!(env.upstream.exchange_calls(), 1);
    let sent = env.upstream.api_body().unwrap();
    assert_eq!(sent["model"], "gpt-5.6-luna");
}

#[tokio::test]
async fn copilot_session_is_cached() {
    let env = TestEnv::build().await;

    for _ in 0..2 {
        let (status, body) = env.chat("claude-sonnet-4-5").await;
        assert_eq!(status, StatusCode::OK, "response body: {body}");
    }

    assert_eq!(
        env.upstream.exchange_calls(),
        1,
        "the second request must reuse the cached session"
    );
    assert_eq!(env.upstream.api_calls(), 2);
}

#[tokio::test]
async fn copilot_session_renewed_when_near_expiry() {
    let env = TestEnv::build().await;
    // First exchange answers a session with under two minutes of life
    // left; the second answers a fresh one.
    env.upstream.push_session("cpt_first", unix_now() + 60);
    env.upstream.push_session("cpt_second", unix_now() + 3600);

    let (status, body) = env.chat("claude-sonnet-4-5").await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(env.upstream.api_auth().as_deref(), Some("Bearer cpt_first"));

    let (status, body) = env.chat("claude-sonnet-4-5").await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(
        env.upstream.exchange_calls(),
        2,
        "a session under two minutes of life must be re-exchanged"
    );
    assert_eq!(
        env.upstream.api_auth().as_deref(),
        Some("Bearer cpt_second")
    );
}

#[tokio::test]
async fn copilot_refused_is_account_error() {
    let env = TestEnv::build().await;
    env.upstream.push_refused();

    let (status, body) = env.chat("claude-sonnet-4-5").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["error"]["type"], "account_error");
    let message = parsed["error"]["message"].as_str().unwrap();
    assert!(message.contains("Copilot"), "message: {message}");
    assert!(message.contains("重新登录"), "message: {message}");
    assert_eq!(
        env.upstream.api_calls(),
        0,
        "the Copilot API must not be called after a refusal"
    );
    assert_eq!(env.upstream.exchange_calls(), 1);
}
