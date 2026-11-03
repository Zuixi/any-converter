//! The account endpoints the window's Accounts page added (P17): the plugin
//! list, the saved account order, and adopt/release of a Codex home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use any_converter_core::convert::Format;
use any_converter_server::config::{
    PassThroughHeadersConfig, ProviderConfig, ServerConfig, ServerSettings,
};
use any_converter_server::credentials::{AccountResolver, FileSecretStore, SecretStore};
use any_converter_server::handlers::AppState;
use any_converter_server::plugins::{ManagedHosts, PluginAuthStore};
use any_converter_server::router::create_router_with_state;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use reqwest::Client;
use serde_json::Value;
use tower::ServiceExt;

fn provider(name: &str, accounts: Vec<String>) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format: Format::OpenAIChat,
        base_url: "http://127.0.0.1:1".into(),
        api_key: "k".into(),
        account: None,
        accounts,
        api: None,
        wire: None,
        plugin: None,
        model_map: HashMap::new(),
        endpoints: Default::default(),
        auth: Default::default(),
        proxy: None,
    }
}

fn app(dir: &std::path::Path, accounts: Vec<String>) -> axum::Router {
    let store = FileSecretStore::new(dir.join("secrets.json"));
    let state = Arc::new(AppState {
        config: ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 0,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![provider("gemini", accounts)],
            model_routes: vec![],
            routes: vec![],
            model_metadata: Default::default(),
            logging: Default::default(),
        },
        client: Client::new(),
        accounts: Arc::new(AccountResolver::new(store, Client::new())),
        copilot: Arc::new(any_converter_server::credentials::CopilotSessions::new(
            Client::new(),
        )),
        copilot_allowances: Arc::new(
            any_converter_server::credentials::CopilotAllowances::default(),
        ),
        hosts: Arc::new(ManagedHosts::with_store(Arc::new(PluginAuthStore::new(
            dir.join("plugin-auth.json"),
        )))),
        usage_logger: None,
        request_logger: None,
    });
    create_router_with_state(state)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(path)
        .method(method)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn plugins_endpoint_answers_an_empty_list_when_none_are_installed() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), Vec::new());
    let (status, body) = call(&app, "GET", "/v1/accounts/plugins", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["plugins"], serde_json::json!([]));
    assert!(value["dir"].as_str().is_some_and(|d| !d.is_empty()));
}

#[tokio::test]
async fn order_is_saved_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = vec!["gemini:first".to_string(), "gemini:second".to_string()];
    let app = app(dir.path(), accounts);

    let (status, body) = call(
        &app,
        "POST",
        "/v1/accounts/order",
        r#"{"provider":"gemini","accounts":["gemini:second","gemini:first"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The saved order is what routing gets: the pool puts it first and any
    // account it doesn't name after.
    let pool = any_converter_server::provider_pool::saved_order(
        &any_converter_server::config::secrets_path(),
        "gemini",
    );
    assert!(pool.is_some_and(|order| order.first().map(String::as_str) == Some("gemini:second")));
}

#[tokio::test]
async fn order_refuses_an_account_of_another_provider() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), vec!["gemini:a".to_string()]);
    let (status, body) = call(
        &app,
        "POST",
        "/v1/accounts/order",
        r#"{"provider":"gemini","accounts":["codex:b"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("not an account of"), "{body}");
}

#[tokio::test]
async fn order_refuses_a_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), vec!["gemini:a".to_string()]);
    let (status, body) = call(
        &app,
        "POST",
        "/v1/accounts/order",
        r#"{"provider":"gemini","accounts":["gemini:a","gemini:a"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("twice"), "{body}");
}

#[tokio::test]
async fn adopt_without_a_codex_sign_in_is_a_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), Vec::new());
    let (status, _body) = call(&app, "POST", "/v1/accounts/codex:nobody/adopt", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn release_without_an_adoption_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), Vec::new());
    let (status, body) = call(&app, "POST", "/v1/accounts/codex:x/release", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["released"], serde_json::json!(false));
}

#[tokio::test]
async fn accounts_list_never_prints_token_material() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSecretStore::new(dir.path().join("secrets.json"));
    store
        .put(
            "gemini:me@example.com",
            &any_converter_server::credentials::Credential::OAuth {
                provider: "gemini".into(),
                account: "me@example.com".into(),
                access_token: "tok-must-not-appear".into(),
                refresh_token: Some("refresh-must-not-appear".into()),
                expires_at_unix: Some(1),
            },
        )
        .unwrap();
    let app = app(dir.path(), vec!["gemini:me@example.com".to_string()]);
    let (status, body) = call(&app, "GET", "/v1/accounts", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("tok-must-not-appear"), "{body}");
    assert!(!body.contains("refresh-must-not-appear"), "{body}");
    assert!(body.contains("gemini:me@example.com"), "{body}");
}

// ── plugin provider sign-in over HTTP (P17) ────────────────────────────

fn node_reachable() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A registry whose plugin hosts point at the fixture package.
fn app_with_plugins(dir: &std::path::Path) -> axum::Router {
    let store = FileSecretStore::new(dir.join("secrets.json"));
    let hosts = Arc::new(ManagedHosts::with_store(Arc::new(PluginAuthStore::new(
        dir.join("plugin-auth.json"),
    ))));
    let state = Arc::new(AppState {
        config: ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 0,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![],
            model_routes: vec![],
            routes: vec![],
            model_metadata: Default::default(),
            logging: Default::default(),
        },
        client: Client::new(),
        accounts: Arc::new(AccountResolver::new(store, Client::new())),
        copilot: Arc::new(any_converter_server::credentials::CopilotSessions::new(
            Client::new(),
        )),
        copilot_allowances: Arc::new(
            any_converter_server::credentials::CopilotAllowances::default(),
        ),
        hosts,
        usage_logger: None,
        request_logger: None,
    });
    create_router_with_state(state)
}

#[tokio::test]
async fn a_plugin_provider_can_be_signed_in_over_http() {
    if !node_reachable() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let spec = format!("{}/tests/fixtures/plugin-hello", env!("CARGO_MANIFEST_DIR"));
    let app = app_with_plugins(dir.path());

    // `plugin:<id>` names the provider; the spec says which package serves it.
    let body = serde_json::json!({ "provider": "plugin:hello", "spec": spec }).to_string();
    let (status, text) = call(&app, "POST", "/v1/accounts/signin", &body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let task: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(task["state"], "waiting");
    assert_eq!(task["provider"], "plugin:hello");
    assert!(
        task["url"].as_str().is_some_and(|u| !u.is_empty()),
        "the plugin's page is reported: {text}"
    );
    // The fixture's one method is a "code" flow.
    assert_eq!(task["paste_code"], true, "{text}");

    let id = task["id"].as_str().unwrap();
    let submit = serde_json::json!({ "input": "code-123" }).to_string();
    let (status, text) = call(
        &app,
        "POST",
        &format!("/v1/accounts/signin/{id}/submit"),
        &submit,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let done: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(done["state"], "done", "{text}");
    assert!(
        done["user"]
            .as_str()
            .is_some_and(|u| u.starts_with("hello:")),
        "the account the plugin kept is reported: {text}"
    );
}

#[tokio::test]
async fn a_plugin_sign_in_without_its_spec_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_plugins(dir.path());
    let body = serde_json::json!({ "provider": "plugin:hello" }).to_string();
    let (status, text) = call(&app, "POST", "/v1/accounts/signin", &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("spec"), "{text}");
}
