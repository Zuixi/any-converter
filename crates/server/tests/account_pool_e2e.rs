//! P9 end-to-end: account pools, quota ordering, and the quotas endpoint.
//!
//! A fake Code Assist upstream stands in for Google: it tells accounts
//! apart by the bearer token (each stored account gets its own), answers
//! `fetchAvailableModels` per token, and records every generate request's
//! Authorization header so tests can assert exactly which account served.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings,
};
use any_converter_server::credentials::{
    AccountResolver, Credential, FileSecretStore, SecretStore,
};
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state;
use axum::Router;
use axum::body::Body;
use axum::extract::{Request as AxumRequest, State as AxumState};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use reqwest::Client;
use serde_json::{Value, json};
use tower::ServiceExt;

const ACCOUNT_A: &str = "gemini:a@example.com";
const ACCOUNT_B: &str = "gemini:b@example.com";

/// Everything the fake Code Assist upstream observed.
#[derive(Default)]
struct FakeUpstream {
    /// `Authorization` header of every generate request, in order.
    generate_auths: Mutex<Vec<String>>,
    /// `Authorization` header of every fetchAvailableModels request.
    quota_auths: Mutex<Vec<String>>,
    /// fetchAvailableModels answers per bearer token: the model's
    /// `remainingFraction`; a token without an entry gets a 404.
    quotas: Mutex<HashMap<String, f64>>,
    /// When set, generate answers this body with 401 for every token.
    generate_401: Mutex<Option<String>>,
}

impl FakeUpstream {
    fn set_quota(&self, token: &str, remaining_fraction: f64) {
        self.quotas
            .lock()
            .unwrap()
            .insert(token.to_string(), remaining_fraction);
    }

    fn set_generate_401(&self, body: &str) {
        *self.generate_401.lock().unwrap() = Some(body.to_string());
    }

    fn generate_auths(&self) -> Vec<String> {
        self.generate_auths.lock().unwrap().clone()
    }

    fn quota_auths(&self) -> Vec<String> {
        self.quota_auths.lock().unwrap().clone()
    }
}

/// The non-streaming Code Assist generate endpoint: record the bearer
/// token, answer an enveloped Gemini response (or the queued 401).
async fn generate_content(
    AxumState(state): AxumState<Arc<FakeUpstream>>,
    req: AxumRequest,
) -> Response {
    state.generate_auths.lock().unwrap().push(auth_of(&req));
    if let Some(body) = state.generate_401.lock().unwrap().clone() {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::from_str::<Value>(&body).unwrap_or(Value::Null)),
        )
            .into_response();
    }
    axum::Json(json!({
        "response": {
            "candidates": [{
                "content": {"parts": [{"text": "hi"}], "role": "model"}
            }]
        }
    }))
    .into_response()
}

/// The Antigravity quota endpoint: answer per bearer token.
async fn fetch_available_models(
    AxumState(state): AxumState<Arc<FakeUpstream>>,
    req: AxumRequest,
) -> Response {
    let auth = auth_of(&req);
    state.quota_auths.lock().unwrap().push(auth.clone());
    let remaining = auth
        .strip_prefix("Bearer ")
        .and_then(|token| state.quotas.lock().unwrap().get(token).copied());
    match remaining {
        Some(fraction) => axum::Json(json!({
            "models": {
                "gemini-3.5-flash": {
                    "quotaInfo": {
                        "remainingFraction": fraction,
                        "resetTime": "2099-01-01T00:00:00Z"
                    }
                }
            }
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": "no quota"})),
        )
            .into_response(),
    }
}

fn auth_of(req: &AxumRequest) -> String {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Spawn the fake upstream on an ephemeral port; returns its base URL.
async fn spawn_upstream() -> (String, Arc<FakeUpstream>) {
    let state = Arc::new(FakeUpstream::default());
    let router = Router::new()
        .route("/v1internal:generateContent", post(generate_content))
        .route(
            "/v1internal:fetchAvailableModels",
            post(fetch_available_models),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), state)
}

/// A plain OpenAI Chat upstream that always answers 200, counting hits.
async fn spawn_openai_upstream() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let state = hits.clone();
    let router = Router::new().route(
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
                        "message": {"role": "assistant", "content": "from-openai"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), state)
}

fn oauth_credential(access_token: &str) -> Credential {
    Credential::OAuth {
        provider: "gemini".into(),
        account: "me@example.com".into(),
        access_token: access_token.into(),
        refresh_token: None,
        expires_at_unix: None,
    }
}

/// A temp secrets.json seeded with accounts A (`tok-a`) and B (`tok-b`).
fn seeded_store(dir: &tempfile::TempDir) -> FileSecretStore {
    let store = FileSecretStore::new(dir.path().join("secrets.json"));
    store.put(ACCOUNT_A, &oauth_credential("tok-a")).unwrap();
    store.put(ACCOUNT_B, &oauth_credential("tok-b")).unwrap();
    store
}

fn gemini_provider(name: &str, base: &str, accounts: Vec<String>) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format: Format::Gemini,
        base_url: base.into(),
        api_key: String::new(),
        account: None,
        accounts,
        api: None,
        wire: None,
        model_map: HashMap::new(),
        endpoints: Default::default(),
        auth: Default::default(),
        plugin: None,
        proxy: None,
    }
}

fn openai_provider(name: &str, base: &str) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format: Format::OpenAIChat,
        base_url: base.into(),
        api_key: "sk-2".into(),
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

fn make_app(providers: Vec<ProviderConfig>, store: FileSecretStore) -> Router {
    let names: Vec<String> = providers.iter().map(|p| p.name.clone()).collect();
    let client = Client::builder().no_proxy().build().unwrap();
    let accounts = Arc::new(AccountResolver::new(store, client.clone()));
    let state = Arc::new(AppState {
        config: ServerConfig {
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
            model_metadata: HashMap::new(),
            logging: Default::default(),
        },
        client,
        accounts,
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
    });
    create_router_with_state(state)
}

/// `GET /v1/accounts/quotas`: the explicit read that primes the routing
/// cache (routing itself never waits on a quota read).
fn quotas_request() -> Request<Body> {
    Request::builder()
        .uri("/v1/accounts/quotas")
        .method("GET")
        .body(Body::empty())
        .unwrap()
}

fn chat_request() -> Request<Body> {
    Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"model":"gemini-3.5-flash","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap()
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn second_account_serves_when_the_first_is_revoked() {
    let (base, upstream) = spawn_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(&dir);
    // A is flagged revoked before the request.
    std::fs::write(
        dir.path().join("revoked.json"),
        json!({ ACCOUNT_A: { "why": "test", "at": 0 } }).to_string(),
    )
    .unwrap();

    let app = make_app(
        vec![gemini_provider(
            "gemini",
            &base,
            vec![ACCOUNT_A.into(), ACCOUNT_B.into()],
        )],
        store,
    );

    let (status, body) = send(&app, chat_request()).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "hi");

    // A never reached the upstream (0 calls with its token); B served.
    assert_eq!(
        upstream.generate_auths(),
        vec!["Bearer tok-b".to_string()],
        "the revoked account must not be tried"
    );
}

#[tokio::test]
async fn all_accounts_fail_then_next_provider() {
    let (gemini_base, gemini_upstream) = spawn_upstream().await;
    gemini_upstream.set_generate_401(r#"{"error":"unauthorized"}"#);
    let (openai_base, openai_hits) = spawn_openai_upstream().await;

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(&dir);
    let app = make_app(
        vec![
            gemini_provider(
                "gemini",
                &gemini_base,
                vec![ACCOUNT_A.into(), ACCOUNT_B.into()],
            ),
            openai_provider("openai", &openai_base),
        ],
        store,
    );

    let (status, body) = send(&app, chat_request()).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "from-openai");

    // Both accounts answered 401 (Unknown) and the request failed over to
    // the second provider; each account was tried exactly once.
    assert_eq!(
        gemini_upstream.generate_auths(),
        vec!["Bearer tok-a".to_string(), "Bearer tok-b".to_string()],
    );
    use std::sync::atomic::Ordering;
    assert_eq!(openai_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn exhausted_account_goes_last() {
    let (base, upstream) = spawn_upstream().await;
    // A is at 99% used (past the 95 line), B at 10%.
    upstream.set_quota("tok-a", 0.01);
    upstream.set_quota("tok-b", 0.9);

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(&dir);
    let app = make_app(
        vec![gemini_provider(
            "gemini",
            &base,
            vec![ACCOUNT_A.into(), ACCOUNT_B.into()],
        )],
        store,
    );

    // Routing never waits on a quota read (magpie reads allowances in the
    // background), so the windows must already be known. The quotas
    // endpoint is the explicit read that primes them — the UI's path.
    let (status, _) = send(&app, quotas_request()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        upstream.quota_auths(),
        vec!["Bearer tok-a".to_string(), "Bearer tok-b".to_string()],
        "the quotas endpoint reads every account"
    );

    let (status, body) = send(&app, chat_request()).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "hi");

    // B (room left) served first; A was never tried because B succeeded.
    assert_eq!(upstream.generate_auths(), vec!["Bearer tok-b".to_string()]);
}

#[tokio::test]
async fn routing_never_reads_quota_on_the_request_path() {
    let (base, upstream) = spawn_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(&dir);
    let app = make_app(
        vec![gemini_provider(
            "gemini",
            &base,
            vec![ACCOUNT_A.into(), ACCOUNT_B.into()],
        )],
        store,
    );

    // Nothing has primed the cache: serving must not go asking. Magpie
    // reads allowances in the background and never waits on one; an
    // account with nothing known counts as having room, so the user's order
    // stands and the request is not delayed by two quota round trips.
    let (status, body) = send(&app, chat_request()).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(
        upstream.quota_auths(),
        Vec::<String>::new(),
        "the request path must not read quotas"
    );
    assert_eq!(upstream.generate_auths(), vec!["Bearer tok-a".to_string()]);
}

#[tokio::test]
async fn unknown_allowance_keeps_order() {
    let (base, upstream) = spawn_upstream().await;
    // The quota endpoint answers 404 for every account: no quota
    // information is known, so the user's order stands.

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(&dir);
    let app = make_app(
        vec![gemini_provider(
            "gemini",
            &base,
            vec![ACCOUNT_A.into(), ACCOUNT_B.into()],
        )],
        store,
    );

    let (status, body) = send(&app, chat_request()).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "hi");

    // A is first in config order and served the request.
    assert_eq!(upstream.generate_auths(), vec!["Bearer tok-a".to_string()]);
}

#[tokio::test]
async fn quotas_endpoint_reports_windows() {
    let (base, upstream) = spawn_upstream().await;
    // A reports one window (42% used); B reports nothing → `[]`, not null.
    upstream.set_quota("tok-a", 0.58);

    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(&dir);
    let app = make_app(
        vec![gemini_provider(
            "gemini",
            &base,
            vec![ACCOUNT_A.into(), ACCOUNT_B.into()],
        )],
        store,
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/accounts/quotas")
                .method("GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    let accounts = body["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);

    let a = &accounts[0];
    assert_eq!(a["provider"], "gemini");
    assert_eq!(a["account"], ACCOUNT_A);
    let a_windows = a["windows"].as_array().unwrap();
    assert_eq!(a_windows.len(), 1);
    assert_eq!(a_windows[0]["name"], "gemini-3.5-flash");
    let used = a_windows[0]["used"].as_f64().unwrap();
    assert!((used - 42.0).abs() < 1e-9, "used = {used}");
    assert_eq!(a_windows[0]["resets"], "2099-01-01T00:00:00Z");

    let b = &accounts[1];
    assert_eq!(b["account"], ACCOUNT_B);
    assert!(
        b["windows"].is_array(),
        "an account without quota information reports [] not null: {}",
        b["windows"]
    );
    assert_eq!(b["windows"].as_array().unwrap().len(), 0);
}
