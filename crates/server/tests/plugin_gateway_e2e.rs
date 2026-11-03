//! End-to-end tests for the plugin provider request path (P6.2 + P6.3).
//!
//! A config provider with `plugin = { spec = <plugin-gateway fixture> }` is
//! served through a real node plugin subprocess (`plugin-host.js`). The
//! fixture answers hermetically and records every request it received into
//! the shared auth ledger (`__gw_seen`), so these tests can assert on the
//! exact url/body/headers the gateway forwarded.
//!
//! Known limitation covered here on purpose: the plugin transport returns
//! the COMPLETE body, so a `stream: true` answer is buffered and then
//! replayed as SSE blocks — this is NOT incremental streaming.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, PluginRef, ProviderConfig, RouteStrategy,
    ServerConfig, ServerSettings,
};
use any_converter_server::credentials::AccountResolver;
use any_converter_server::handlers::AppState;
use any_converter_server::plugins::{
    ApiKeyParams, ManagedHosts, PluginAuth, PluginAuthStore, ProviderPlugin,
};
use any_converter_server::router::create_router_with_state;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use reqwest::Client;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Absolute path of the fixture plugin (spec = path, target = "").
fn gateway_spec() -> PluginRef {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-gateway");
    PluginRef {
        spec: dir.to_string_lossy().into_owned(),
        target: String::new(),
    }
}

fn node_program() -> String {
    std::env::var("ANY_CONVERTER_NODE").unwrap_or_else(|_| "node".to_string())
}

fn node_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new(node_program())
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

/// A provider served by the gateway fixture plugin. `base_url`/`api_key`
/// stay empty on purpose: the plugin's loader supplies them.
fn plugin_provider(name: &str, account: &str) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format: Format::OpenAIChat,
        base_url: String::new(),
        api_key: String::new(),
        account: Some(account.into()),
        accounts: Vec::new(),
        api: None,
        wire: None,
        model_map: HashMap::new(),
        endpoints: Default::default(),
        auth: Default::default(),
        plugin: Some(gateway_spec()),
        proxy: None,
    }
}

/// A plain http provider used as the failover target.
fn http_provider(name: &str, base_url: &str) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format: Format::OpenAIChat,
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

/// Test fixture: a router with the given providers plus the plugin host
/// manager sharing one auth store (tempdir-backed).
struct Harness {
    _temp: tempfile::TempDir,
    hosts: Arc<ManagedHosts>,
    store: Arc<PluginAuthStore>,
    app: Router,
}

impl Harness {
    async fn new(providers: Vec<ProviderConfig>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(PluginAuthStore::new(temp.path().join("auth.json")));
        let hosts = Arc::new(ManagedHosts::with_store(Arc::clone(&store)));
        let names: Vec<String> = providers.iter().map(|p| p.name.clone()).collect();
        let config = ServerConfig {
            pricing: Default::default(),
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
        };
        let state = Arc::new(AppState {
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
            hosts: Arc::clone(&hosts),
        });
        let app = create_router_with_state(state);
        Self {
            _temp: temp,
            hosts,
            store,
            app,
        }
    }

    /// Store an api-key credential for `account` through the plugin's own
    /// `apiKey` RPC (the JS host persists it into the shared auth ledger;
    /// the fixture's loader only runs for accounts with stored credentials).
    async fn sign_in(&self, account: &str, inputs: Option<BTreeMap<String, Value>>) {
        let host = self.hosts.host_for(&gateway_spec()).await.unwrap();
        let result = ProviderPlugin::api_key(
            &*host,
            ApiKeyParams {
                provider: host.info().id.clone(),
                method: 0,
                inputs,
                key: "sk-test".into(),
                account: account.into(),
                proxy: None,
            },
        )
        .await
        .unwrap();
        assert!(result.ok, "plugin apiKey failed: {result:?}");
    }

    async fn send(&self, method: &str, path: &str, body: &str) -> (StatusCode, String, String) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .method(method)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            content_type,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    async fn post(&self, path: &str, body: &str) -> (StatusCode, String, String) {
        self.send("POST", path, body).await
    }

    /// Every request the fixture plugin received, in order.
    fn seen(&self) -> Vec<Value> {
        match self.store.load().unwrap().get("__gw_seen") {
            Some(PluginAuth::ApiKey { key, .. }) => serde_json::from_str(key).unwrap(),
            other => panic!("__gw_seen entry missing or wrong shape: {other:?}"),
        }
    }
}

/// A minimal http upstream answering every chat completion identically.
async fn spawn_http_upstream(content: &'static str) -> String {
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            axum::Json(json!({
                "id": "backup-1",
                "object": "chat.completion",
                "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn chat_client_to_chat_model_is_byte_identical() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    let h = Harness::new(vec![plugin_provider("gw", "test")]).await;
    h.sign_in("test", None).await;

    // Keys are pre-sorted so the gateway's JSON round trips (private-field
    // stripping + model patch) re-serialize this body byte-for-byte. This is
    // the zero-conversion proof: Chat client → Chat model.
    let sent = r#"{"max_tokens":16,"messages":[{"content":"hello gw","role":"user"}],"model":"gw-chat","stream":false,"temperature":0.7}"#;

    let (status, _, body) = h.post("/v1/chat/completions", sent).await;
    assert_eq!(status, StatusCode::OK);

    let seen = h.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["body"], sent);
    assert_eq!(seen[0]["model"], "gw-chat");
    assert_eq!(seen[0]["npm"], "@ai-sdk/openai-compatible");

    // The plugin's answer is converted back to the client format.
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "hi from gw chat");
}

#[tokio::test]
async fn claude_client_to_claude_model_is_byte_identical() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    let h = Harness::new(vec![plugin_provider("gw", "test")]).await;
    h.sign_in("test", None).await;

    let sent = r#"{"max_tokens":32,"messages":[{"content":"hello gw claude","role":"user"}],"model":"gw-claude","stream":false,"system":"be terse"}"#;

    let (status, _, body) = h.post("/v1/messages", sent).await;
    assert_eq!(status, StatusCode::OK);

    // Zero-conversion proof: Claude client → Claude model.
    let seen = h.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["body"], sent);
    assert_eq!(seen[0]["npm"], "@ai-sdk/anthropic");

    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["content"][0]["text"], "hi from gw claude");
}

#[tokio::test]
async fn chat_client_to_claude_model_converts_once() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    let h = Harness::new(vec![plugin_provider("gw", "test")]).await;
    h.sign_in("test", None).await;

    let (status, _, body) = h
        .post(
            "/v1/chat/completions",
            r#"{"messages":[{"content":"convert me","role":"user"}],"model":"gw-claude","stream":false}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // The plugin received a Claude-shaped body: `messages` + mandatory
    // `max_tokens`, none of the Chat-specific `choices`/assistant-role
    // structure.
    let seen = h.seen();
    assert_eq!(seen.len(), 1);
    let upstream: Value = serde_json::from_str(seen[0]["body"].as_str().unwrap()).unwrap();
    assert!(
        upstream.get("messages").is_some(),
        "expected messages: {upstream}"
    );
    assert!(
        upstream.get("max_tokens").is_some(),
        "expected max_tokens: {upstream}"
    );
    assert!(
        upstream.get("choices").is_none(),
        "unexpected choices: {upstream}"
    );
    let roles: Vec<&str> = upstream["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["role"].as_str())
        .collect();
    assert!(
        !roles.contains(&"assistant"),
        "client request must not carry assistant turns: {upstream}"
    );

    // And the Claude answer came back as a Chat response.
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        value["choices"][0]["message"]["content"],
        "hi from gw claude"
    );
}

#[tokio::test]
async fn plugin_url_is_base_plus_api_path() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    let h = Harness::new(vec![plugin_provider("gw", "test")]).await;
    h.sign_in("test", None).await;

    let _ = h
        .post(
            "/v1/chat/completions",
            r#"{"messages":[{"content":"hi","role":"user"}],"model":"gw-chat","stream":false}"#,
        )
        .await;
    let _ = h
        .post(
            "/v1/messages",
            r#"{"max_tokens":8,"messages":[{"content":"hi","role":"user"}],"model":"gw-claude","stream":false}"#,
        )
        .await;
    let _ = h
        .post(
            "/v1/responses",
            r#"{"input":"hi","model":"gw-responses","stream":false}"#,
        )
        .await;

    // url = loader.baseURL + TrimPrefix(path, "/v1"), as magpie's
    // pluginFetch does: the AI SDK's base ends where magpie's /v1 does, so a
    // base of https://gw.example.test asks for /chat/completions.
    let seen = h.seen();
    let urls: Vec<&str> = seen
        .iter()
        .map(|seen| seen["url"].as_str().unwrap())
        .collect();
    assert_eq!(
        urls,
        vec![
            "https://gw.example.test/chat/completions",
            "https://gw.example.test/messages",
            "https://gw.example.test/responses",
        ]
    );
}

#[tokio::test]
async fn loader_base_ending_in_v1_is_not_doubled() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    // A loader base that already carries the version (as @ai-sdk/anthropic's
    // npmBase does: https://api.anthropic.com/v1) must not become
    // /v1/v1/messages. magpie pluginFetch strips the leading /v1 from the
    // path for exactly this reason.
    let h = Harness::new(vec![plugin_provider("gw", "test")]).await;
    // The `versioned` metadata makes the fixture's loader return a base that
    // already ends in /v1 (as @ai-sdk/anthropic's npmBase does).
    h.sign_in(
        "test",
        Some(BTreeMap::from([("versioned".to_string(), json!(true))])),
    )
    .await;
    let (status, _, body) = h
        .post(
            "/v1/messages",
            r#"{"max_tokens":8,"messages":[{"content":"hi","role":"user"}],"model":"gw-claude","stream":false}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let seen = h.seen();
    assert_eq!(seen.len(), 1, "one request reached the plugin");
    assert_eq!(
        seen[0]["url"].as_str().unwrap(),
        "https://gw.example.test/v1/messages"
    );
}

#[tokio::test]
async fn plugin_models_list_is_used_for_the_model_list() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    let h = Harness::new(vec![plugin_provider("gw", "test")]).await;

    let (status, _, body) = h.send("GET", "/v1/models", "").await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body).unwrap();
    let ids: Vec<&str> = value["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    for id in ["gw-chat", "gw-claude", "gw-responses"] {
        assert!(ids.contains(&id), "missing {id} in /v1/models: {ids:?}");
    }
}

#[tokio::test]
async fn plugin_relative_url_outside_base_is_rejected() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    // The `relative` metadata makes the fixture's loader return a hostless
    // base URL; the same-host guard must reject it (a credentialed request
    // whose host the gateway cannot verify). Genuine cross-host cases are
    // covered by `proxy::assert_same_host` unit tests.
    let h = Harness::new(vec![plugin_provider("evil", "evil")]).await;
    h.sign_in(
        "evil",
        Some(BTreeMap::from([("relative".to_string(), json!(true))])),
    )
    .await;

    let (status, _, body) = h
        .post(
            "/v1/chat/completions",
            r#"{"messages":[{"content":"hi","role":"user"}],"model":"gw-chat","stream":false}"#,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body.contains("host"),
        "expected a host-mismatch message, got: {body}"
    );
}

#[tokio::test]
async fn plugin_streaming_is_buffered_then_replayed() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    // Documented limitation: the plugin returns the COMPLETE body, so the
    // "streaming" answer below is buffered by the gateway and replayed as
    // SSE blocks in one shot — it is NOT incremental streaming.
    let h = Harness::new(vec![plugin_provider("gw", "test")]).await;
    h.sign_in("test", None).await;

    let (status, content_type, body) = h
        .post(
            "/v1/chat/completions",
            r#"{"messages":[{"content":"stream me","role":"user"}],"model":"gw-chat","stream":true}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type, "text/event-stream");
    assert!(
        body.contains("hi from gw chat"),
        "expected the replayed content, got: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "expected a terminating [DONE], got: {body}"
    );
}

#[tokio::test]
async fn plugin_failure_fails_over() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    // First provider is the plugin (it answers 500 for a marked message);
    // the second is a plain http provider that must take over.
    let backup = spawn_http_upstream("from-http-backup").await;
    let h = Harness::new(vec![
        plugin_provider("gw", "test"),
        http_provider("backup", &backup),
    ])
    .await;
    h.sign_in("test", None).await;

    let (status, _, body) = h
        .post(
            "/v1/chat/completions",
            r#"{"messages":[{"content":"TRIGGER_FAIL_500","role":"user"}],"model":"gw-chat","stream":false}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        value["choices"][0]["message"]["content"],
        "from-http-backup"
    );

    // The plugin saw exactly one attempt before the gateway failed over.
    assert_eq!(h.seen().len(), 1);
}
