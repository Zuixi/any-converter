//! The account endpoints the window's Accounts page added (P17): the plugin
//! list, the saved account order, and adopt/release of a Codex home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, PluginRef, ProviderConfig, RouteStrategy,
    ServerConfig, ServerSettings,
};
use any_converter_server::control_plane::ControlPlane;
use any_converter_server::credentials::copilot::CopilotSessions;
use any_converter_server::credentials::{
    AccountResolver, Credential, FileSecretStore, SecretStore, SiwcMeta,
};
use any_converter_server::handlers::AppState;
use any_converter_server::home::Home;
use any_converter_server::plugins::{ManagedHosts, PluginAuth, PluginAuthStore};
use any_converter_server::router::create_router_with_control_plane;
use any_converter_server::secrets::{
    ApiKeyStore, KEYCHAIN_SERVICE, KeychainBackend, MemoryKeychain,
};
use axum::body::Body;
use axum::extract::Request as AxumRequest;
use axum::extract::State as AxumState;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use reqwest::Client;
use serde_json::Value;
use tower::ServiceExt;

const MANAGEMENT_KEY: &str = "test-management-key";

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
            pricing: Default::default(),
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
        agents: Arc::new(any_converter_server::agents::AgentRegistry::in_memory()),
        usage_logger: None,
        request_logger: None,
    });
    managed_router(state, dir)
}

fn config_with(
    providers: Vec<ProviderConfig>,
    model_routes: Vec<ModelRouteConfig>,
) -> ServerConfig {
    ServerConfig {
        pricing: Default::default(),
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 0,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        },
        providers,
        model_routes,
        routes: vec![],
        model_metadata: Default::default(),
        logging: Default::default(),
    }
}

fn app_with_config(
    dir: &std::path::Path,
    config: ServerConfig,
    copilot_token_url: Option<String>,
) -> axum::Router {
    let store = FileSecretStore::new(dir.join("secrets.json"));
    let client = Client::builder().no_proxy().build().unwrap();
    let accounts = Arc::new(AccountResolver::new(store, client.clone()));
    let copilot = CopilotSessions::new(client.clone());
    let copilot = match copilot_token_url {
        Some(url) => copilot.with_token_url(&url),
        None => copilot,
    };
    let state = Arc::new(AppState {
        config,
        client,
        accounts,
        copilot: Arc::new(copilot),
        copilot_allowances: Arc::new(
            any_converter_server::credentials::CopilotAllowances::default(),
        ),
        hosts: Arc::new(ManagedHosts::with_store(Arc::new(PluginAuthStore::new(
            dir.join("plugin-auth.json"),
        )))),
        agents: Arc::new(any_converter_server::agents::AgentRegistry::in_memory()),
        usage_logger: None,
        request_logger: None,
    });
    managed_router(state, dir)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(path)
        .method(method)
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {MANAGEMENT_KEY}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn managed_router(state: Arc<AppState>, dir: &std::path::Path) -> axum::Router {
    let home_root = dir.join("control-home");
    std::fs::create_dir_all(&home_root).unwrap();
    std::fs::write(
        home_root.join("config.toml"),
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
        Home::from_root(home_root),
        ApiKeyStore::with_backend(KEYCHAIN_SERVICE, std::sync::Arc::new(keychain)),
    )
    .unwrap();
    create_router_with_control_plane(state, prepared.control_plane)
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
                siwc: None,
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

// ── account model discovery ────────────────────────────────────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn put_oauth(dir: &std::path::Path, id: &str, credential: Credential) {
    FileSecretStore::new(dir.join("secrets.json"))
        .put(id, &credential)
        .unwrap();
}

fn route_to(pattern: &str, provider: &str) -> ModelRouteConfig {
    ModelRouteConfig {
        pattern: pattern.into(),
        provider: Some(provider.into()),
        providers: Vec::new(),
        upstream_model: None,
        strategy: RouteStrategy::Priority,
    }
}

#[derive(Clone)]
struct FakeModels {
    status: StatusCode,
    body: Value,
    auth: Arc<Mutex<Vec<String>>>,
}

async fn fake_models(AxumState(state): AxumState<FakeModels>, request: AxumRequest) -> Response {
    state.auth.lock().unwrap().push(
        request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    if state.status.is_success() {
        (state.status, Json(state.body.clone())).into_response()
    } else {
        (
            state.status,
            Json(serde_json::json!({ "error": "upstream unavailable" })),
        )
            .into_response()
    }
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    base
}

async fn spawn_fake_models(status: StatusCode, body: Value) -> (String, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = FakeModels {
        status,
        body,
        auth: Arc::clone(&seen),
    };
    let base = serve(
        Router::new()
            .route("/v1/models", get(fake_models))
            .with_state(state),
    )
    .await;
    (base, seen)
}

#[tokio::test]
async fn account_models_resolves_siwc_credential_and_lists_chatgpt_models() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen_auth) = spawn_fake_models(
        StatusCode::OK,
        serde_json::json!({ "data": [{ "id": "gpt-5" }] }),
    )
    .await;
    let id = "chatgpt-api:me@example.com";
    put_oauth(
        dir.path(),
        id,
        Credential::OAuth {
            provider: "chatgpt-api".into(),
            account: "me@example.com".into(),
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at_unix: Some(unix_now() + 3600),
            // ChatGPT API credentials carry this field. Resolution used to
            // accept only `siwc: None`, so such an account could be listed
            // but never have its models discovered.
            siwc: Some(SiwcMeta {
                client_id: "client-1".into(),
                ..Default::default()
            }),
        },
    );
    let mut p = provider("chatgpt-api", vec![id.into()]);
    p.base_url = base;
    let app = app_with_config(dir.path(), config_with(vec![p], vec![]), None);

    let (status, body) = call(&app, "GET", &format!("/v1/accounts/{id}/models"), "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["models"][0]["id"], "gpt-5", "{body}");
    assert_eq!(value["source"], "chatgpt-api", "{body}");
    assert_eq!(value["bound"], true, "{body}");
    assert!(value["error"].is_null(), "{body}");
    assert_eq!(
        seen_auth.lock().unwrap().as_slice(),
        ["Bearer access".to_string()],
        "the stored access token was used"
    );
}

#[tokio::test]
async fn provider_name_hint_discovers_models_without_claiming_binding() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = spawn_fake_models(
        StatusCode::OK,
        serde_json::json!({ "data": [{ "id": "gpt-5" }] }),
    )
    .await;
    let id = "chatgpt-api:me@example.com";
    put_oauth(
        dir.path(),
        id,
        Credential::OAuth {
            provider: "chatgpt-api".into(),
            account: "me@example.com".into(),
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at_unix: Some(unix_now() + 3600),
            siwc: None,
        },
    );
    // Same provider name and base URL, but it does not name the account.
    let mut p = provider("chatgpt-api", Vec::new());
    p.base_url = base;
    let app = app_with_config(dir.path(), config_with(vec![p], vec![]), None);

    let (status, body) = call(&app, "GET", &format!("/v1/accounts/{id}/models"), "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["models"][0]["id"], "gpt-5", "{body}");
    assert_eq!(value["bound"], false, "{body}");
}

#[tokio::test]
async fn account_models_merge_dynamic_and_literal_configured_models_once() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = spawn_fake_models(
        StatusCode::OK,
        serde_json::json!({
            "data": [{ "id": "gpt-5" }, { "id": "shared-model" }]
        }),
    )
    .await;
    let id = "chatgpt-api:me@example.com";
    put_oauth(
        dir.path(),
        id,
        Credential::OAuth {
            provider: "chatgpt-api".into(),
            account: "me@example.com".into(),
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at_unix: Some(unix_now() + 3600),
            siwc: None,
        },
    );
    let mut p = provider("chatgpt-api", vec![id.into()]);
    p.base_url = base;
    p.model_map
        .insert("alias-model".into(), "upstream-alias".into());
    p.model_map
        .insert("shared-model".into(), "upstream-shared".into());
    p.model_map.insert("gpt-*".into(), "upstream-gpt".into());
    let config = config_with(
        vec![p],
        vec![
            route_to("literal-model", "chatgpt-api"),
            route_to("wildcard-*", "chatgpt-api"),
        ],
    );
    let app = app_with_config(dir.path(), config, None);

    let (status, body) = call(&app, "GET", &format!("/v1/accounts/{id}/models"), "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    let models = value["models"].as_array().unwrap();
    assert_eq!(models.len(), 4, "{body}");
    let sources: HashMap<&str, &str> = models
        .iter()
        .map(|model| {
            (
                model["id"].as_str().unwrap(),
                model["source"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        sources.len(),
        models.len(),
        "no model id was repeated: {body}"
    );
    assert_eq!(sources.get("shared-model"), Some(&"chatgpt-api"));
    assert_eq!(sources.get("alias-model"), Some(&"configured"));
    assert_eq!(sources.get("literal-model"), Some(&"configured"));
    assert!(!sources.contains_key("wildcard-*"), "{body}");
    assert!(!sources.contains_key("gpt-*"), "{body}");
}

#[tokio::test]
async fn failed_discovery_keeps_literal_configured_models() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = spawn_fake_models(StatusCode::INTERNAL_SERVER_ERROR, Value::Null).await;
    let id = "chatgpt-api:me@example.com";
    put_oauth(
        dir.path(),
        id,
        Credential::OAuth {
            provider: "chatgpt-api".into(),
            account: "me@example.com".into(),
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at_unix: Some(unix_now() + 3600),
            siwc: None,
        },
    );
    let mut p = provider("chatgpt-api", vec![id.into()]);
    p.base_url = base;
    p.model_map
        .insert("kept-model".into(), "upstream-kept".into());
    let app = app_with_config(dir.path(), config_with(vec![p], vec![]), None);

    let (status, body) = call(&app, "GET", &format!("/v1/accounts/{id}/models"), "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert!(!value["error"].is_null(), "{body}");
    assert_eq!(value["models"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(value["models"][0]["id"], "kept-model", "{body}");
    assert_eq!(value["models"][0]["source"], "configured", "{body}");
}

#[tokio::test]
async fn a_bound_account_without_literal_models_reports_unsupported_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let id = "codex:me@example.com";
    put_oauth(
        dir.path(),
        id,
        Credential::OAuth {
            provider: "codex".into(),
            account: "me@example.com".into(),
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at_unix: Some(unix_now() + 3600),
            siwc: None,
        },
    );
    let config = config_with(
        vec![provider("codex", vec![id.into()])],
        vec![route_to("gpt-*", "codex")],
    );
    let app = app_with_config(dir.path(), config, None);

    let (status, body) = call(&app, "GET", &format!("/v1/accounts/{id}/models"), "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["source"], "unsupported", "{body}");
    assert_eq!(value["bound"], true, "{body}");
    assert_eq!(value["models"].as_array().unwrap().len(), 0, "{body}");
    assert!(value["error"].is_null(), "{body}");
}

#[derive(Clone)]
struct FakeCopilot {
    api_base: Arc<Mutex<String>>,
    exchange_auth: Arc<Mutex<String>>,
    models_auth: Arc<Mutex<String>>,
}

async fn fake_copilot_token(
    AxumState(state): AxumState<FakeCopilot>,
    request: AxumRequest,
) -> Response {
    *state.exchange_auth.lock().unwrap() = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    Json(serde_json::json!({
        "token": "cpt-session",
        "expires_at": unix_now() + 3600,
        "endpoints": { "api": state.api_base.lock().unwrap().as_str() },
    }))
    .into_response()
}

async fn fake_copilot_models(
    AxumState(state): AxumState<FakeCopilot>,
    request: AxumRequest,
) -> Response {
    *state.models_auth.lock().unwrap() = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    Json(serde_json::json!({
        "data": [{ "id": "claude-sonnet-4-5" }]
    }))
    .into_response()
}

#[tokio::test]
async fn account_models_exchanges_a_copilot_session_before_listing_models() {
    let dir = tempfile::tempdir().unwrap();
    let id = "copilot:octocat";
    put_oauth(
        dir.path(),
        id,
        Credential::OAuth {
            provider: "copilot".into(),
            account: "octocat".into(),
            access_token: "gho_test".into(),
            refresh_token: None,
            expires_at_unix: Some(unix_now() + 3600),
            siwc: None,
        },
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = FakeCopilot {
        api_base: Arc::new(Mutex::new(base.clone())),
        exchange_auth: Arc::new(Mutex::new(String::new())),
        models_auth: Arc::new(Mutex::new(String::new())),
    };
    let router = Router::new()
        .route("/copilot_internal/v2/token", get(fake_copilot_token))
        .route("/models", get(fake_copilot_models))
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let p = provider("copilot", vec![id.into()]);
    let app = app_with_config(
        dir.path(),
        config_with(vec![p], vec![]),
        Some(format!("{base}/copilot_internal/v2/token")),
    );
    let (status, body) = call(&app, "GET", &format!("/v1/accounts/{id}/models"), "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["models"][0]["id"], "claude-sonnet-4-5", "{body}");
    assert_eq!(value["source"], "copilot", "{body}");
    assert_eq!(value["bound"], true, "{body}");
    assert_eq!(
        state.exchange_auth.lock().unwrap().as_str(),
        "token gho_test"
    );
    assert_eq!(
        state.models_auth.lock().unwrap().as_str(),
        "Bearer cpt-session"
    );
}

#[tokio::test]
async fn account_models_unknown_account_is_a_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), Vec::new());
    let (status, body) = call(&app, "GET", "/v1/accounts/missing/models", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("account_not_found"), "{body}");
}

#[tokio::test]
async fn accounts_list_includes_plugin_accounts_without_exposing_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = PluginAuthStore::new(dir.path().join("plugin-auth.json"));
    store
        .set(
            "hello:acct",
            PluginAuth::ApiKey {
                key: "plugin-secret-must-not-appear".into(),
                extra: BTreeMap::new(),
            },
        )
        .unwrap();
    store
        .set(
            "__gw_seen",
            PluginAuth::ApiKey {
                key: "internal-must-not-appear".into(),
                extra: BTreeMap::new(),
            },
        )
        .unwrap();

    let app = app(dir.path(), Vec::new());
    let (status, body) = call(&app, "GET", "/v1/accounts", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    let accounts = value["accounts"].as_array().unwrap();
    let account = accounts
        .iter()
        .find(|account| account["id"] == "hello:acct")
        .unwrap_or_else(|| panic!("plugin account missing: {body}"));
    assert_eq!(account["source"], "plugin", "{body}");
    assert_eq!(account["provider"], "hello", "{body}");
    assert!(!body.contains("plugin-secret-must-not-appear"), "{body}");
    assert!(!body.contains("internal-must-not-appear"), "{body}");
    assert!(!body.contains("__gw_seen"), "{body}");
}

#[tokio::test]
async fn plugin_account_models_are_discovered_through_its_configured_provider() {
    if !node_reachable() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = PluginAuthStore::new(dir.path().join("plugin-auth.json"));
    store
        .set(
            "hello:acct",
            PluginAuth::OAuth {
                refresh: Some("refresh".into()),
                access: Some("access".into()),
                expires: Some(unix_now() + 3600),
                extra: BTreeMap::new(),
            },
        )
        .unwrap();

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugin-hello")
        .to_string_lossy()
        .into_owned();
    let mut p = provider("hello", vec!["hello:acct".into()]);
    p.base_url = String::new();
    p.api_key = String::new();
    p.plugin = Some(PluginRef {
        spec: fixture,
        target: String::new(),
    });
    let app = app_with_config(dir.path(), config_with(vec![p], vec![]), None);

    let (status, body) = call(&app, "GET", "/v1/accounts/hello:acct/models", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["models"][0]["id"], "hello-chat", "{body}");
    assert_eq!(value["models"][0]["name"], "Hello Chat", "{body}");
    assert_eq!(value["source"], "plugin", "{body}");
    assert_eq!(value["bound"], true, "{body}");
    assert!(value["error"].is_null(), "{body}");
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
            pricing: Default::default(),
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
        agents: Arc::new(any_converter_server::agents::AgentRegistry::in_memory()),
        copilot_allowances: Arc::new(
            any_converter_server::credentials::CopilotAllowances::default(),
        ),
        hosts,
        usage_logger: None,
        request_logger: None,
    });
    managed_router(state, dir)
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

/// Listing plugins must not start one Node runtime per package every time
/// the Accounts page is opened: what a package serves is remembered in
/// `plugin-providers.json`, and only an unknown or changed package is
/// loaded. Ten plugins must not mean ten runtimes per page load.
#[tokio::test]
async fn plugin_listing_remembers_what_a_package_serves() {
    if !node_reachable() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    let spec_dir = plugins.join("node_modules").join("hello");
    std::fs::create_dir_all(&spec_dir).unwrap();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-hello");
    for entry in std::fs::read_dir(&fixture).unwrap().flatten() {
        let _ = std::fs::copy(entry.path(), spec_dir.join(entry.file_name()));
    }

    let hosts = ManagedHosts::with_store(Arc::new(PluginAuthStore::new(
        dir.path().join("plugin-auth.json"),
    )));

    let first = hosts.installed(&plugins).await;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].spec, "hello");
    assert_eq!(first[0].providers[0].id, "hello", "{:?}", first[0].error);

    // Remembered next to the secrets file, so the next listing — and the
    // next run of the server — answers without starting the package.
    let known = dir.path().join("plugin-providers.json");
    assert!(
        known.exists() || any_converter_server::plugins::managed::known_path().exists(),
        "what a package serves is remembered"
    );

    // A second listing of the same, unchanged package still answers.
    let second = hosts.installed(&plugins).await;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].providers[0].id, "hello");
}
