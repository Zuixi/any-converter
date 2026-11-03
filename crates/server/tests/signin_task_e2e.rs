//! End-to-end tests for the P10 task-based sign-in API, the account list
//! endpoint and sign-out removal:
//!
//! 1. `POST /v1/accounts/signin` starts a task and reports `waiting` at once;
//! 2. `POST .../submit` with a state-mismatched callback finishes without
//!    panicking (no 401s either);
//! 3. `POST .../cancel` → 204 and the task is gone (GET → 404);
//! 4. `GET /v1/accounts` never leaks tokens;
//! 5. `POST /v1/accounts/{id}/remove` without a vendor revocation endpoint
//!    still removes the account and reports `NotOffered`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

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
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use reqwest::Client;
use serde_json::{Value, json};
use tower::ServiceExt;

/// One OAuth credential with a distinctive token value (assertions check
/// that the value never appears in an API response).
fn oauth_credential(provider: &str, account: &str, access_token: &str) -> Credential {
    Credential::OAuth {
        provider: provider.into(),
        account: account.into(),
        access_token: access_token.into(),
        refresh_token: Some("1//refresh-secret-value".into()),
        expires_at_unix: Some(1_900_000_000),
        siwc: None,
    }
}

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
            name: "gemini".into(),
            format: Format::Gemini,
            base_url: "https://cloudcode-pa.googleapis.com".into(),
            api_key: String::new(),
            account: None,
            accounts: vec!["gemini:me@example.com".into()],
            api: None,
            wire: None,
            model_map: HashMap::new(),
            endpoints: Default::default(),
            auth: Default::default(),
            plugin: None,
            proxy: None,
        }],
        model_routes: vec![ModelRouteConfig {
            pattern: "*".into(),
            provider: Some("gemini".into()),
            providers: vec![],
            upstream_model: None,
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: HashMap::new(),
        logging: LoggingConfig::default(),
    }
}

/// A temp secrets store plus the router wired over it.
struct TestEnv {
    _dir: tempfile::TempDir,
    app: Router,
}

impl TestEnv {
    fn build() -> Self {
        Self::build_with(&[(
            "gemini:me@example.com",
            oauth_credential("gemini", "me@example.com", "ya29.access-token-secret-value"),
        )])
    }

    fn build_with(seeds: &[(&str, Credential)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"));
        for (id, credential) in seeds {
            store.put(id, credential).unwrap();
        }

        let client = Client::builder().no_proxy().build().unwrap();
        let accounts = Arc::new(AccountResolver::new(store, client.clone()));
        let state = Arc::new(AppState {
            config: server_config(),
            client,
            accounts,
            copilot: Arc::new(CopilotSessions::new(Client::new())),
            copilot_allowances: Arc::new(
                any_converter_server::credentials::CopilotAllowances::default(),
            ),
            usage_logger: None,
            request_logger: None,
            agents: Arc::new(any_converter_server::agents::AgentRegistry::in_memory()),
            hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
                Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                    std::env::temp_dir().join("any-converter-test-signin-plugin-auth.json"),
                )),
            )),
        });
        let app = create_router_with_state(state);
        Self { _dir: dir, app }
    }

    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let request = Request::builder()
            .uri(path)
            .method(method)
            .header("content-type", "application/json")
            .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed)
    }

    async fn signin_gemini(&self) -> Value {
        let (status, body) = self
            .send(
                "POST",
                "/v1/accounts/signin",
                Some(json!({ "provider": "gemini" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "response body: {body}");
        body
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn signin_task_starts_and_reports_waiting() {
    let env = TestEnv::build();
    let task = env.signin_gemini().await;

    assert_eq!(task["state"], "waiting");
    assert!(
        task["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?")),
        "url: {}",
        task["url"]
    );
    assert_eq!(task["paste_callback"], true);
    assert_eq!(task["provider"], "gemini");

    // Poll once: still waiting (no callback arrived).
    let id = task["id"].as_str().unwrap();
    let (status, body) = env
        .send("GET", &format!("/v1/accounts/signin/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "waiting");

    // Tidy up so no 10-minute waiter outlives the test.
    let (status, _) = env
        .send("POST", &format!("/v1/accounts/signin/{id}/cancel"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn signin_task_submit_finishes() {
    let env = TestEnv::build();
    let task = env.signin_gemini().await;
    let id = task["id"].as_str().unwrap();

    // A forged callback URL whose state cannot match the generated one.
    let bogus = "http://127.0.0.1:1/oauth2callback?state=forged-state&code=auth-code";
    let (status, body) = env
        .send(
            "POST",
            &format!("/v1/accounts/signin/{id}/submit"),
            Some(json!({ "input": bogus })),
        )
        .await;
    // No 401/500/404: the submit either finishes (failed) or stays waiting.
    assert!(
        status.is_success() || status == StatusCode::BAD_REQUEST,
        "unexpected status: {status}, body: {body}"
    );
    assert_ne!(status, StatusCode::UNAUTHORIZED);
    let state = body["state"].as_str().unwrap_or("");
    assert!(
        state == "failed" || state == "waiting",
        "unexpected state: {state}, body: {body}"
    );
}

#[tokio::test]
async fn signin_task_cancel_removes_it() {
    let env = TestEnv::build();
    let task = env.signin_gemini().await;
    let id = task["id"].as_str().unwrap();

    let (status, _) = env
        .send("POST", &format!("/v1/accounts/signin/{id}/cancel"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The task is removed from the registry: GET now 404s.
    let (status, _) = env
        .send("GET", &format!("/v1/accounts/signin/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn accounts_list_never_leaks_tokens() {
    let env = TestEnv::build();

    let (status, body) = env.send("GET", "/v1/accounts", None).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");

    let accounts = body["accounts"].as_array().expect("accounts array");
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], "gemini:me@example.com");
    assert_eq!(accounts[0]["kind"], "oauth");
    assert_eq!(accounts[0]["provider"], "gemini");
    assert_eq!(accounts[0]["account"], "me@example.com");

    // The serialized response must never carry token material.
    let raw = body.to_string();
    assert!(
        !raw.contains("ya29.access-token-secret-value"),
        "access token leaked: {raw}"
    );
    assert!(
        !raw.contains("1//refresh-secret-value"),
        "refresh token leaked: {raw}"
    );
    assert!(!raw.contains("access_token"), "token field leaked: {raw}");
    assert!(!raw.contains("refresh_token"), "token field leaked: {raw}");
}

#[tokio::test]
async fn remove_without_revocation_endpoint_still_removes() {
    // Copilot/GitHub publish no revocation endpoint: the removal must still
    // delete the account locally and report `NotOffered`.
    let env = TestEnv::build_with(&[(
        "copilot:wdjb****",
        oauth_credential("copilot", "wdjb****", "gho_access_secret_value"),
    )]);

    let (status, body) = env
        .send(
            "POST",
            "/v1/accounts/copilot:wdjb****/remove",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(body["outcome"], "NotOffered");

    // The account is gone from the list.
    let (status, body) = env.send("GET", "/v1/accounts", None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|account| account["id"].as_str())
        .collect();
    assert!(
        !ids.contains(&"copilot:wdjb****"),
        "copilot account still listed: {ids:?}"
    );
}
