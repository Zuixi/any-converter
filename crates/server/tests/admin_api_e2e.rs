#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use any_converter_core::convert::Format;
use any_converter_server::config::ServerConfig;
use any_converter_server::control_plane::{ConfigPatch, ControlPlane, ProviderPatch, ServerPatch};
use any_converter_server::credentials::AccountResolver;
use any_converter_server::credentials::copilot::CopilotSessions;
use any_converter_server::handlers::AppState;
use any_converter_server::home::Home;
use any_converter_server::router::create_router_with_control_plane;
use any_converter_server::secrets::{
    ApiKeyStore, KEYCHAIN_SERVICE, KeychainBackend, MemoryKeychain,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use reqwest::Client;
use tower::ServiceExt;

fn temp_home(tag: &str) -> Home {
    let root = std::env::temp_dir().join(format!(
        "any-converter-admin-api-e2e-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    Home::from_root(root)
}

fn state(config: ServerConfig) -> Arc<AppState> {
    let client = Client::new();
    Arc::new(AppState {
        config,
        client: client.clone(),
        accounts: AccountResolver::with_default_store(client.clone()),
        copilot: Arc::new(CopilotSessions::new(client.clone())),
        copilot_allowances: Arc::default(),
        usage_logger: None,
        request_logger: None,
        hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
            Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                std::env::temp_dir().join("any-converter-admin-api-plugin-auth.json"),
            )),
        )),
        agents: Arc::new(any_converter_server::agents::AgentRegistry::in_memory()),
    })
}

async fn send(
    app: axum::Router,
    method: &str,
    path: &str,
    body: &str,
    management_key: Option<&str>,
    if_match: Option<&str>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json");
    if let Some(key) = management_key {
        builder = builder.header("Authorization", format!("Bearer {key}"));
    }
    if let Some(revision) = if_match {
        builder = builder.header("If-Match", format!("\"{revision}\""));
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn admin_config_uses_management_auth_etag_and_typed_patches() {
    let home = temp_home("config");
    let keychain = Arc::new(MemoryKeychain::new());
    let keys = ApiKeyStore::with_backend(KEYCHAIN_SERVICE, keychain.clone());
    let prepared = ControlPlane::prepare(home.clone(), keys).unwrap();
    let management_key = prepared.control_plane.management_key();
    let runtime_config = prepared.config.clone();
    let control_plane = Arc::clone(&prepared.control_plane);
    let app = create_router_with_control_plane(state(runtime_config), control_plane);

    let response = send(app.clone(), "GET", "/v1/admin/config", "", None, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = send(
        app.clone(),
        "GET",
        "/v1/admin/config",
        "",
        Some("wrong"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = send(
        app.clone(),
        "GET",
        "/v1/admin/config",
        "",
        Some(&management_key),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| panic!("missing ETag: {:?}", response.headers()))
        .trim_matches('"')
        .to_string();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let view: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(view["revision"], etag.as_str());
    assert!(
        view["server"]["management_key_configured"]
            .as_bool()
            .unwrap_or(false),
        "unexpected config view: {view}"
    );

    let response = send(
        app.clone(),
        "PATCH",
        "/v1/admin/config",
        "{}",
        Some(&management_key),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_REQUIRED);

    keychain
        .set(KEYCHAIN_SERVICE, "provider:openai", "sk-upstream")
        .unwrap();
    let patch = ConfigPatch {
        server: Some(ServerPatch {
            port: Some(1229),
            ..Default::default()
        }),
        upsert_providers: vec![ProviderPatch {
            name: "openai".into(),
            format: Some(Format::OpenAIChat),
            base_url: Some("https://api.example.test".into()),
            api_key_ref: None,
            model_map: None,
            account: None,
            accounts: None,
            auth_scheme: None,
            proxy: None,
        }],
        ..Default::default()
    };
    let response = send(
        app.clone(),
        "PATCH",
        "/v1/admin/config",
        &serde_json::to_string(&patch).unwrap(),
        Some(&management_key),
        Some(&etag),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let updated: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(updated["server"]["port"], 1229);
    assert_eq!(updated["restart_required"], true);

    let source = std::fs::read_to_string(home.config_path()).unwrap();
    assert!(source.contains("keychain:any-converter:provider:openai"));
    assert!(!source.contains("sk-upstream"));

    let _ = std::fs::remove_dir_all(home.root());
}

#[tokio::test]
async fn secret_and_rotation_endpoints_use_separate_authorities() {
    let home = temp_home("secrets");
    let keychain = Arc::new(MemoryKeychain::new());
    let keys = ApiKeyStore::with_backend(KEYCHAIN_SERVICE, keychain.clone());
    let prepared = ControlPlane::prepare(home.clone(), keys.clone()).unwrap();
    let original_management_key = prepared.control_plane.management_key();
    let runtime_config = prepared.config.clone();
    let control_plane = Arc::clone(&prepared.control_plane);
    let app = create_router_with_control_plane(state(runtime_config), control_plane);

    let initial = send(app.clone(), "GET", "/v1/admin/config", "", None, None).await;
    assert_eq!(initial.status(), StatusCode::UNAUTHORIZED);

    keys.set_inference("inference-only").unwrap();
    let forbidden = send(
        app.clone(),
        "GET",
        "/v1/admin/config",
        "",
        Some("inference-only"),
        None,
    )
    .await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let current = send(
        app.clone(),
        "GET",
        "/v1/admin/config",
        "",
        Some(&original_management_key),
        None,
    )
    .await;
    let revision = current
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .trim_matches('"')
        .to_string();

    let stale_patch = send(
        app.clone(),
        "PATCH",
        "/v1/admin/config",
        "{}",
        Some(&original_management_key),
        Some("0".repeat(64).as_str()),
    )
    .await;
    assert_eq!(stale_patch.status(), StatusCode::PRECONDITION_FAILED);

    let patch = ConfigPatch {
        upsert_providers: vec![ProviderPatch {
            name: "openai".into(),
            format: Some(Format::OpenAIChat),
            base_url: Some("https://api.example.test".into()),
            api_key_ref: None,
            model_map: None,
            account: None,
            accounts: None,
            auth_scheme: None,
            proxy: None,
        }],
        ..Default::default()
    };
    let created = send(
        app.clone(),
        "PATCH",
        "/v1/admin/config",
        &serde_json::to_string(&patch).unwrap(),
        Some(&original_management_key),
        Some(&revision),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created_revision = created
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .trim_matches('"')
        .to_string();

    let provider_secret = send(
        app.clone(),
        "PUT",
        "/v1/admin/secrets/provider/openai",
        r#"{"api_key":"sk-provider"}"#,
        Some(&original_management_key),
        Some(&created_revision),
    )
    .await;
    assert_eq!(provider_secret.status(), StatusCode::OK);

    let inference_secret = send(
        app.clone(),
        "PUT",
        "/v1/admin/secrets/inference",
        r#"{"api_key":"sk-inference"}"#,
        Some(&original_management_key),
        None,
    )
    .await;
    assert_eq!(inference_secret.status(), StatusCode::PRECONDITION_REQUIRED);

    let provider_result = axum::body::to_bytes(provider_secret.into_body(), usize::MAX)
        .await
        .unwrap();
    let provider_result: serde_json::Value = serde_json::from_slice(&provider_result).unwrap();
    let after_provider = provider_result["revision"].as_str().unwrap().to_string();
    let inference_secret = send(
        app.clone(),
        "PUT",
        "/v1/admin/secrets/inference",
        r#"{"api_key":"sk-inference"}"#,
        Some(&original_management_key),
        Some(&after_provider),
    )
    .await;
    assert_eq!(inference_secret.status(), StatusCode::OK);

    let rotated = send(
        app.clone(),
        "POST",
        "/v1/admin/secrets/management/rotate",
        "",
        Some(&original_management_key),
        None,
    )
    .await;
    assert_eq!(rotated.status(), StatusCode::OK);
    assert_ne!(keys.get_management().unwrap(), original_management_key);

    let old_key_rejected = send(
        app.clone(),
        "GET",
        "/v1/admin/config",
        "",
        Some(&original_management_key),
        None,
    )
    .await;
    assert_eq!(old_key_rejected.status(), StatusCode::FORBIDDEN);

    let _ = std::fs::remove_dir_all(home.root());
}
