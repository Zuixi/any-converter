//! End-to-end tests for the Gemini dynamic model list (P8): Code Assist's
//! `loadCodeAssist` reply drives `/v1/models` through the experiment flags,
//! the found project is cached for an hour, a missing project falls back to
//! the static table with a warning, and an unreachable upstream falls back
//! the same way — all against a fake Code Assist upstream.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings, UpstreamApi,
};
use any_converter_server::credentials::{
    AccountError, AccountResolver, Credential, FileSecretStore, GEMINI_MODELS, SecretStore,
};
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state;
use axum::body::Body;
use axum::extract::Request as AxumRequest;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, extract::State as AxumState, routing::post};
use reqwest::Client;
use serde_json::{Value, json};
use tower::ServiceExt;

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ── Fake Code Assist upstream ────────────────────────────────────────────

/// What `loadCodeAssist` answers.
#[derive(Debug, Clone, Copy, Default)]
enum LoadReply {
    /// A set-up account: `currentTier` + project + experiments
    /// (latestFlash on).
    #[default]
    Models,
    /// No `currentTier`; the default tier bills a user-named project.
    NeedsProject,
    /// The upstream is broken.
    Unreachable,
}

#[derive(Default)]
struct FakeCodeAssist {
    reply: Mutex<LoadReply>,
    load_calls: Mutex<usize>,
    load_auth: Mutex<Option<String>>,
    load_body: Mutex<Option<Value>>,
}

impl FakeCodeAssist {
    fn set_reply(&self, reply: LoadReply) {
        *self.reply.lock().unwrap() = reply;
    }

    fn load_calls(&self) -> usize {
        *self.load_calls.lock().unwrap()
    }

    fn load_auth(&self) -> Option<String> {
        self.load_auth.lock().unwrap().clone()
    }

    fn load_body(&self) -> Option<Value> {
        self.load_body.lock().unwrap().clone()
    }
}

async fn load_code_assist(
    AxumState(state): AxumState<Arc<FakeCodeAssist>>,
    req: AxumRequest,
) -> Response {
    *state.load_calls.lock().unwrap() += 1;
    *state.load_auth.lock().unwrap() = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .unwrap();
    *state.load_body.lock().unwrap() = serde_json::from_slice(&body).ok();

    match *state.reply.lock().unwrap() {
        LoadReply::Models => Json(json!({
            "currentTier": {"id": "pro-tier", "name": "Gemini Pro"},
            "cloudaicompanionProject": {"id": "fake-project", "name": "Fake Project"},
            "allowedTiers": [{"id": "pro-tier", "isDefault": true}],
            "experiments": [
                {"flagId": 45842815, "boolValue": true},
                {"flagId": 45760185, "boolValue": false}
            ]
        }))
        .into_response(),
        LoadReply::NeedsProject => Json(json!({
            "allowedTiers": [{
                "id": "standard-tier",
                "userDefinedCloudaicompanionProject": true
            }],
            "ineligibleTiers": [{"reasonMessage": "billing account required"}]
        }))
        .into_response(),
        LoadReply::Unreachable => (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
    }
}

/// Spawn the fake upstream on an ephemeral port; returns its base URL.
async fn spawn_upstream() -> (String, Arc<FakeCodeAssist>) {
    let state = Arc::new(FakeCodeAssist::default());
    let router = Router::new()
        .route("/v1internal:loadCodeAssist", post(load_code_assist))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), state)
}

// ── Warn capture ──────────────────────────────────────────────────────────

static LOGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static LOGGER_INIT: Once = Once::new();

struct RecordingLogger;

impl log::Log for RecordingLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if let Some(logs) = LOGS.get() {
            logs.lock()
                .unwrap()
                .push(format!("{}: {}", record.level(), record.args()));
        }
    }

    fn flush(&self) {}
}

/// Install the recording logger (first call wins; later calls reuse it).
fn init_logger() {
    LOGGER_INIT.call_once(|| {
        let _ = log::set_boxed_logger(Box::new(RecordingLogger));
        log::set_max_level(log::LevelFilter::Debug);
    });
}

/// Take the logs recorded so far.
fn drain_logs() -> Vec<String> {
    std::mem::take(&mut *LOGS.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap())
}

// ── Test scaffolding ──────────────────────────────────────────────────────

fn code_assist_provider(base: &str) -> ProviderConfig {
    ProviderConfig {
        name: "code-assist".into(),
        format: Format::Gemini,
        base_url: base.into(),
        api_key: String::new(),
        account: Some("gemini:me@example.com".into()),
        accounts: Vec::new(),
        api: Some(UpstreamApi::CodeAssist),
        wire: None,
        model_map: Default::default(),
        endpoints: Default::default(),
        auth: Default::default(),
        plugin: None,
        proxy: None,
    }
}

struct TestEnv {
    _dir: tempfile::TempDir,
    app: Router,
    accounts: Arc<AccountResolver>,
    upstream: Arc<FakeCodeAssist>,
    base: String,
}

impl TestEnv {
    async fn build() -> Self {
        let (base, upstream) = spawn_upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let store = FileSecretStore::new(secrets_path);
        store
            .put(
                "gemini:me@example.com",
                &Credential::OAuth {
                    provider: "gemini".into(),
                    account: "me@example.com".into(),
                    access_token: "tok-1".into(),
                    refresh_token: Some("ref-1".into()),
                    expires_at_unix: Some(unix_now() + 3600),
                    siwc: None,
                },
            )
            .unwrap();

        let client = Client::builder().no_proxy().build().unwrap();
        let accounts = Arc::new(AccountResolver::with_token_url(
            store,
            client.clone(),
            format!("{base}/token"),
        ));

        let state = Arc::new(AppState {
            config: ServerConfig {
                server: ServerSettings {
                    host: "127.0.0.1".into(),
                    port: 8080,
                    api_key: None,
                    pass_through_headers: PassThroughHeadersConfig::default(),
                    proxy: None,
                },
                providers: vec![code_assist_provider(&base)],
                model_routes: vec![ModelRouteConfig {
                    pattern: "gemini-oauth".into(),
                    provider: Some("code-assist".into()),
                    providers: vec![],
                    upstream_model: None,
                    strategy: RouteStrategy::Priority,
                }],
                routes: vec![],
                model_metadata: Default::default(),
                logging: Default::default(),
            },
            client,
            accounts: Arc::clone(&accounts),
            copilot_allowances: Arc::new(
                any_converter_server::credentials::CopilotAllowances::default(),
            ),
            usage_logger: None,
            request_logger: None,
            copilot: Arc::new(any_converter_server::credentials::CopilotSessions::new(
                Client::new(),
            )),
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
            accounts,
            upstream,
            base,
        }
    }

    /// GET /v1/models → (status, parsed body).
    async fn list_models(&self) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .method("GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

fn model_ids(body: &Value) -> Vec<String> {
    body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap().to_string())
        .collect()
}

fn assert_full_static_table(ids: &[String]) {
    assert_eq!(GEMINI_MODELS.len(), 10, "the static table has 10 models");
    for (id, _) in GEMINI_MODELS {
        assert!(
            ids.iter().any(|listed| listed == id),
            "static model {id} missing from /v1/models: {ids:?}"
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn code_assist_models_come_from_the_flags() {
    let env = TestEnv::build().await;
    env.upstream.set_reply(LoadReply::Models);

    let (status, body) = env.list_models().await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let ids = model_ids(&body);
    // latestFlash=true: 3.8 Flash replaces 3.5 Flash (and 2.5 Flash).
    assert!(
        ids.iter().any(|id| id == "gemini-3.8-flash"),
        "ids: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id == "gemini-3.5-flash"),
        "ids: {ids:?}"
    );

    // The discovery call was signed with the account's OAuth token and
    // carried Gemini CLI's metadata.
    assert_eq!(env.upstream.load_auth().as_deref(), Some("Bearer tok-1"));
    let sent = env.upstream.load_body().unwrap();
    assert_eq!(sent["metadata"]["pluginType"], "GEMINI", "sent: {sent}");
    assert_eq!(sent["metadata"]["ideType"], "IDE_UNSPECIFIED");
}

#[tokio::test]
async fn code_assist_project_is_cached() {
    let env = TestEnv::build().await;
    env.upstream.set_reply(LoadReply::Models);

    for _ in 0..2 {
        let (status, body) = env.list_models().await;
        assert_eq!(status, StatusCode::OK, "response body: {body}");
    }

    // The second ask rides the one-hour project cache, not the wire.
    assert_eq!(env.upstream.load_calls(), 1);
}

#[tokio::test]
async fn code_assist_without_project_returns_actionable_error() {
    init_logger();
    let env = TestEnv::build().await;
    env.upstream.set_reply(LoadReply::NeedsProject);
    drain_logs();

    // /v1/models still succeeds, on the full static table.
    let (status, body) = env.list_models().await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_full_static_table(&model_ids(&body));

    // The failure was warned about, naming the missing project.
    let logs = drain_logs();
    assert!(
        logs.iter()
            .any(|entry| entry.starts_with("WARN") && entry.contains("project")),
        "expected a warn naming the project, got: {logs:?}"
    );

    // project() itself surfaces the actionable error, with Google's reason.
    let err = env
        .accounts
        .project("gemini:me@example.com", &env.base, None)
        .await
        .unwrap_err();
    match err {
        AccountError::NeedsProject(msg) => {
            assert!(msg.contains("project"), "message: {msg}");
            assert!(
                msg.contains("billing account required"),
                "Google's reason must ride along: {msg}"
            );
        }
        other => panic!("expected NeedsProject, got {other}"),
    }
}

#[tokio::test]
async fn code_assist_unreachable_falls_back_to_static() {
    let env = TestEnv::build().await;
    env.upstream.set_reply(LoadReply::Unreachable);

    let (status, body) = env.list_models().await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_full_static_table(&model_ids(&body));
    assert_eq!(env.upstream.load_calls(), 1);
}
