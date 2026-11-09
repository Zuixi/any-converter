#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use any_converter_server::agents::AgentRegistry;
use any_converter_server::config::ServerConfig;
use any_converter_server::control_plane::ControlPlane;
use any_converter_server::credentials::copilot::CopilotSessions;
use any_converter_server::credentials::{AccountResolver, CopilotAllowances, FileSecretStore};
use any_converter_server::handlers::AppState;
use any_converter_server::home::Home;
use any_converter_server::plugins::{ManagedHosts, PluginAuthStore};
use any_converter_server::router::create_router_with_control_plane;
use any_converter_server::secrets::{
    ApiKeyStore, KEYCHAIN_SERVICE, KeychainBackend, MemoryKeychain,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use reqwest::Client;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

const GATEWAY: &str = "http://127.0.0.1:8080";
const MANAGEMENT_KEY: &str = "test-management-key";

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    data_dir: PathBuf,
    app: axum::Router,
}

impl Fixture {
    fn new(api_key: Option<&str>, provider: &str, model: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let data_dir = temp.path().join("data");
        std::fs::create_dir_all(&home).unwrap();
        let config = config(api_key, provider, model);
        let registry = build_registry(&data_dir, api_key);
        registry.set_home(home.clone());
        let app = app_with(config, registry, &data_dir);
        Self {
            _temp: temp,
            home,
            data_dir,
            app,
        }
    }

    fn with_config(&self, api_key: Option<&str>, provider: &str, model: &str) -> axum::Router {
        let registry = build_registry(&self.data_dir, api_key);
        registry.set_home(self.home.clone());
        app_with(config(api_key, provider, model), registry, &self.data_dir)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }

    fn state(&self) -> Value {
        let bytes = std::fs::read(self.data_dir.join("agent_state.json")).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}

fn build_registry(data_dir: &Path, api_key: Option<&str>) -> Arc<AgentRegistry> {
    let store = Arc::new(FileSecretStore::new(data_dir.join("secrets.json")));
    Arc::new(AgentRegistry::new(
        data_dir.to_path_buf(),
        store,
        GATEWAY.to_owned(),
        api_key.map(String::from),
    ))
}

fn app_with(config: ServerConfig, registry: Arc<AgentRegistry>, data_dir: &Path) -> axum::Router {
    let control_home = data_dir.join("control-home");
    std::fs::create_dir_all(&control_home).unwrap();
    std::fs::write(
        control_home.join("config.toml"),
        concat!(
            "[server]\nhost = \"127.0.0.1\"\nport = 1228\n",
            "[management]\napi_key = \"keychain:any-converter:server:management\"\n"
        ),
    )
    .unwrap();
    let keychain = MemoryKeychain::new();
    keychain
        .set(KEYCHAIN_SERVICE, "server:management", MANAGEMENT_KEY)
        .unwrap();
    let prepared = ControlPlane::prepare(
        Home::from_root(control_home),
        ApiKeyStore::with_backend(KEYCHAIN_SERVICE, Arc::new(keychain)),
    )
    .unwrap();
    let state = Arc::new(AppState {
        config,
        client: Client::builder().no_proxy().build().unwrap(),
        accounts: AccountResolver::with_default_store(Client::new()),
        copilot: Arc::new(CopilotSessions::new(Client::new())),
        copilot_allowances: Arc::new(CopilotAllowances::default()),
        usage_logger: None,
        request_logger: None,
        hosts: Arc::new(ManagedHosts::with_store(Arc::new(PluginAuthStore::new(
            std::env::temp_dir().join(format!(
                "any-converter-agents-e2e-plugin-auth-{}.json",
                uuid_like()
            )),
        )))),
        agents: registry,
    });
    create_router_with_control_plane(state, prepared.control_plane)
}

fn uuid_like() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    )
}

fn config(api_key: Option<&str>, provider: &str, model: &str) -> ServerConfig {
    let api_key_line = api_key
        .map(|key| format!("api_key = \"{key}\"\n"))
        .unwrap_or_default();
    let toml = format!(
        r#"
[server]
host = "127.0.0.1"
port = 8080
{api_key_line}
[[providers]]
name = "{provider}"
format = "openai_chat"
base_url = "http://127.0.0.1:1"
api_key = "upstream-key"

[providers.model_map]
"{model}" = "{model}"

[[model_routes]]
pattern = "*"
provider = "{provider}"
"#
    );
    ServerConfig::from_toml(&toml).unwrap()
}

fn seed_all_agents(home: &Path) {
    write(home.join(".claude/settings.json"), "{}\n");
    write(home.join(".codex/config.toml"), "model = \"old\"\n");
    write(home.join(".gemini/.env"), "OLD=1\n");
    write(home.join(".pi/agent/settings.json"), "{}\n");
}

fn write(path: PathBuf, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<&str>,
    auth: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        builder = builder.header("Authorization", auth);
    } else if path.starts_with("/v1/agents") {
        builder = builder.header("Authorization", format!("Bearer {MANAGEMENT_KEY}"));
    }
    let body = body.unwrap_or_default().to_owned();
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

async fn send_without_auth(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.unwrap_or_default().to_owned()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

fn agent_status<'a>(body: &'a Value, id: &str) -> &'a str {
    body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .map(|row| row["status"].as_str().unwrap())
        .unwrap()
}

fn claude_key(home: &Path) -> String {
    let text = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    value["env"]["ANTHROPIC_AUTH_TOKEN"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn list_reports_all_agents_absent_in_empty_home() {
    let fixture = Fixture::new(None, "deepseek", "deepseek-chat");
    let (status, body) = send(&fixture.app, "GET", "/v1/agents", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&body);
    assert_eq!(body["agents"].as_array().unwrap().len(), 4);
    for id in ["claude-code", "codex", "gemini-cli", "pi"] {
        assert_eq!(agent_status(&body, id), "absent", "{id}");
    }
}

#[tokio::test]
async fn connect_disconnect_roundtrip_is_byte_identical() {
    let fixture = Fixture::new(None, "deepseek", "claude-sonnet-4");
    seed_all_agents(&fixture.home);
    let settings = fixture.path(".claude/settings.json");
    // Non-empty user content makes the post-extraction document semantically
    // equal to the backup, exercising the byte-faithful restore path.
    write(
        settings.clone(),
        "{\n  \"env\": {\n    \"FOO\": \"bar\"\n  }\n}\n",
    );
    let before = std::fs::read(&settings).unwrap();

    let (status, body) = send(&fixture.app, "GET", "/v1/agents", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent_status(&json(&body), "claude-code"), "disconnected");

    let connect_body = r#"{"model":"deepseek/claude-sonnet-4"}"#;
    let (status, body) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/connect",
        Some(connect_body),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let written = std::fs::read_to_string(&settings).unwrap();
    assert!(written.contains(GATEWAY), "{written}");
    assert!(written.contains("acw_claude-code_"), "{written}");
    assert!(written.contains("deepseek/claude-sonnet-4"), "{written}");
    assert_eq!(
        fixture.state()["agents"]["claude-code"]["status"],
        "connected"
    );
    assert_eq!(
        fixture.state()["agents"]["claude-code"]["default_model"],
        "deepseek/claude-sonnet-4"
    );

    let (status, body) = send(&fixture.app, "GET", "/v1/agents", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent_status(&json(&body), "claude-code"), "connected");

    let (status, body) = send(
        &fixture.app,
        "GET",
        "/v1/agents/claude-code/keys",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !String::from_utf8_lossy(&body).contains("acw_"),
        "keys endpoint leaked the plaintext key"
    );
    let body = json(&body);
    assert_eq!(body["keys"][0]["id"], "ak_claude-code_01");
    assert!(body["keys"][0].get("key").is_none());

    let (status, body) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/disconnect",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(std::fs::read(&settings).unwrap(), before);
    assert_eq!(
        fixture.state()["agents"]["claude-code"]["status"],
        "disconnected"
    );
}

#[tokio::test]
async fn auth_matrix_global_and_agent_keys() {
    let fixture = Fixture::new(Some("client-secret"), "deepseek", "claude-sonnet-4");
    seed_all_agents(&fixture.home);
    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/connect",
        Some("{}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let agent_key = claude_key(&fixture.home);
    let inference = r#"{"model":"deepseek/claude-sonnet-4","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#;

    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/messages",
        Some(inference),
        Some("Bearer client-secret"),
    )
    .await;
    assert_ne!(status, StatusCode::UNAUTHORIZED, "global key rejected");

    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/messages",
        Some(inference),
        Some(&format!("Bearer {agent_key}")),
    )
    .await;
    assert_ne!(status, StatusCode::UNAUTHORIZED, "agent key rejected");

    let (status, _) = send(&fixture.app, "POST", "/v1/messages", Some(inference), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/sync",
        None,
        Some("Bearer client-secret"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "inference key must not manage agents"
    );

    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/sync",
        None,
        Some(&format!("Bearer {agent_key}")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "agent key must not manage agents"
    );

    let (status, _) =
        send_without_auth(&fixture.app, "POST", "/v1/agents/claude-code/sync", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn local_mode_is_open_without_a_global_key() {
    let fixture = Fixture::new(None, "deepseek", "claude-sonnet-4");
    seed_all_agents(&fixture.home);
    let (status, _) = send(&fixture.app, "GET", "/v1/agents", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/connect",
        Some("{}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn list_detects_drift_after_manual_edit() {
    let fixture = Fixture::new(None, "deepseek", "claude-sonnet-4");
    seed_all_agents(&fixture.home);
    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/connect",
        Some("{}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let settings = fixture.path(".claude/settings.json");
    let text = std::fs::read_to_string(&settings).unwrap();
    std::fs::write(
        &settings,
        text.replace(GATEWAY, "https://api.anthropic.com"),
    )
    .unwrap();

    let (status, body) = send(&fixture.app, "GET", "/v1/agents", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent_status(&json(&body), "claude-code"), "drifted");
}

#[tokio::test]
async fn sync_rewrites_agent_after_provider_change() {
    let fixture = Fixture::new(None, "deepseek", "deepseek-chat");
    seed_all_agents(&fixture.home);
    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/connect",
        Some("{}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let settings = fixture.path(".claude/settings.json");
    assert!(
        std::fs::read_to_string(&settings)
            .unwrap()
            .contains("deepseek/deepseek-chat")
    );

    let app = fixture.with_config(None, "kimi", "kimi-k2");
    let (status, body) = send(&app, "POST", "/v1/agents/claude-code/sync", None, None).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let text = std::fs::read_to_string(&settings).unwrap();
    assert!(text.contains("kimi/kimi-k2"), "{text}");
    assert!(!text.contains("deepseek/deepseek-chat"), "{text}");
}

#[tokio::test]
async fn unknown_agent_is_404() {
    let fixture = Fixture::new(None, "deepseek", "deepseek-chat");
    let (status, body) = send(&fixture.app, "GET", "/v1/agents/nope/keys", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&body)["error"]["code"], "unknown_agent");

    let (status, body) = send(&fixture.app, "POST", "/v1/agents/nope/sync", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&body)["error"]["code"], "unknown_agent");
}

#[tokio::test]
async fn drift_reconciliation_ignores_third_party_proxy_and_catches_key_change() {
    // The applied-audit reconciliation replaces the URL heuristic: a base
    // URL pointing at a THIRD-PARTY proxy (not the gateway, not a known
    // upstream) is still drift, but the audit also catches key-only edits
    // and never misreports an unrelated key addition.
    let fixture = Fixture::new(None, "deepseek", "claude-sonnet-4");
    seed_all_agents(&fixture.home);
    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/connect",
        Some("{}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let settings = fixture.path(".claude/settings.json");

    // 1. Unrelated key added by the user: NOT drift (audit compares only
    //    the keys we wrote).
    let text = std::fs::read_to_string(&settings).unwrap();
    let added = text.replace(
        "\"env\": {",
        "\"mcpServers\": {\"x\": {\"command\": \"y\"}},\n  \"env\": {",
    );
    std::fs::write(&settings, &added).unwrap();
    let (_, body) = send(&fixture.app, "GET", "/v1/agents", None, None).await;
    assert_eq!(
        agent_status(&json(&body), "claude-code"),
        "connected",
        "unrelated user key must not be drift"
    );

    // 2. Auth token swapped by another tool: drift (the audit records the
    //    exact value we wrote).
    let swapped = added.replace("acw_claude-code_", "acw_swapped_");
    std::fs::write(&settings, &swapped).unwrap();
    let (_, body) = send(&fixture.app, "GET", "/v1/agents", None, None).await;
    assert_eq!(agent_status(&json(&body), "claude-code"), "drifted");
}

#[tokio::test]
async fn drift_falls_back_to_heuristic_without_audit() {
    // Agents wired before the audit existed (or with agent_applied.json
    // deleted) still get the heuristic check: a connected agent whose base
    // URL no longer matches the gateway is reported drifted.
    let fixture = Fixture::new(None, "deepseek", "claude-sonnet-4");
    seed_all_agents(&fixture.home);
    let (status, _) = send(
        &fixture.app,
        "POST",
        "/v1/agents/claude-code/connect",
        Some("{}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    std::fs::remove_file(fixture.data_dir.join("agent_applied.json")).unwrap();

    let settings = fixture.path(".claude/settings.json");
    let text = std::fs::read_to_string(&settings).unwrap();
    std::fs::write(
        &settings,
        text.replace(GATEWAY, "https://api.deepseek.com/anthropic"),
    )
    .unwrap();

    let (_, body) = send(&fixture.app, "GET", "/v1/agents", None, None).await;
    assert_eq!(agent_status(&json(&body), "claude-code"), "drifted");
}
