use std::path::PathBuf;
use std::sync::Arc;

use any_converter_core::convert::Format;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post, put},
};
use reqwest::Client;

use axum::Extension;

use crate::admin::{self, AdminRuntime};
use crate::agents::{
    AgentRegistry, catalog_from_config, handle_agent_connect, handle_agent_disconnect,
    handle_agent_keys, handle_agent_rotate_key, handle_agent_sync, handle_agents_list,
};
use crate::config::ServerConfig;
use crate::control_plane::{ControlPlane, ManagementAuthority};
use crate::credentials::AccountResolver;
use crate::credentials::CopilotAllowances;
use crate::credentials::FileSecretStore;
use crate::credentials::copilot::CopilotSessions;
use crate::handlers::{
    AppState, handle_account_adopt, handle_account_models, handle_account_order,
    handle_account_plugins, handle_account_quotas, handle_account_release, handle_account_remove,
    handle_plugin_install, handle_plugin_uninstall,
    handle_account_signin, handle_account_signin_cancel, handle_account_signin_status,
    handle_account_signin_submit, handle_accounts, handle_admin_convert, handle_admin_logs,
    handle_admin_status, handle_admin_usage, handle_claude, handle_count_tokens, handle_gemini,
    handle_health, handle_model_retrieve, handle_models, handle_openai_chat, handle_responses,
    handle_usage_api,
};
use crate::plugins::{ManagedHosts, PluginAuthStore};
use crate::signin_task::SignInTasks;
use crate::web_auth::{self, WebAuth};
use crate::web_ui;

/// Route metadata detected from the request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteInfo {
    pub client_format: Format,
    pub path_streaming: bool,
}

/// Detect client format from a request path.
///
/// Returns `None` for special endpoints like `/health` and `/v1/models`.
pub fn detect_format_from_path(path: &str) -> Option<RouteInfo> {
    match path {
        "/v1/chat/completions" | "/chat/completions" => Some(RouteInfo {
            client_format: Format::OpenAIChat,
            path_streaming: false,
        }),
        "/v1/messages" | "/messages" => Some(RouteInfo {
            client_format: Format::Claude,
            path_streaming: false,
        }),
        "/v1/responses" | "/responses" => Some(RouteInfo {
            client_format: Format::OpenAIResponses,
            path_streaming: false,
        }),
        path if path.starts_with("/v1beta/models/") => parse_gemini_path(path),
        _ => None,
    }
}

fn parse_gemini_path(path: &str) -> Option<RouteInfo> {
    let rest = path.strip_prefix("/v1beta/models/")?;
    if rest.ends_with(":streamGenerateContent") {
        Some(RouteInfo {
            client_format: Format::Gemini,
            path_streaming: true,
        })
    } else if rest.ends_with(":generateContent") {
        Some(RouteInfo {
            client_format: Format::Gemini,
            path_streaming: false,
        })
    } else {
        None
    }
}

/// Extract the model name from a Gemini path.
pub fn extract_gemini_model(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1beta/models/")?;
    if let Some(model) = rest.strip_suffix(":streamGenerateContent") {
        Some(model.to_string())
    } else {
        rest.strip_suffix(":generateContent")
            .map(|model| model.to_string())
    }
}

/// Where plugin credentials live.
///
/// Plugin sign-ins are persisted by the JS plugin host at whatever path the
/// `init` handshake hands it (through `client.auth.set`); [`ManagedHosts`]
/// passes this very path. It sits next to the OAuth account store but in
/// its own file: the two ledgers have different shapes and writers.
fn plugin_auth_path() -> PathBuf {
    crate::config::secrets_path().with_file_name("plugin-auth.json")
}

pub fn create_router(config: ServerConfig) -> Router {
    create_router_with_state(build_app_state(config))
}

/// Build the process-local runtime state owned by the server crate.
pub fn build_app_state(config: ServerConfig) -> Arc<AppState> {
    let log_dir = config.logging.dir.clone();
    let sqlite = crate::storage::open_sqlite_storage_for_log_dir(log_dir.as_deref());
    // The JSONL usage file is an operator convenience mirror, not the store:
    // sqlite is the single query source. Only write it when asked to.
    let usage_logger = if config.logging.jsonl_mirror {
        log_dir
            .as_ref()
            .map(|dir| Arc::new(crate::usage::UsageLogger::new(PathBuf::from(dir))))
    } else {
        None
    };
    let request_logger = if config.logging.request_log.enabled {
        log_dir.as_ref().map(|dir| {
            Arc::new(crate::request_log::RequestLogger::with_sqlite(
                PathBuf::from(dir),
                sqlite.clone(),
            ))
        })
    } else {
        None
    };

    if let Some(ref dir) = log_dir {
        let max_bytes = config.logging.max_disk_mb * 1024 * 1024;
        let policy = crate::storage::RetentionPolicy {
            rollup_days: config.logging.rollup_retention_days,
            log_days: config.logging.retention_days,
            body_days: config.logging.body_retention_days,
        };
        let _disk_quota = crate::disk_quota::spawn_disk_quota_manager(
            PathBuf::from(dir),
            max_bytes,
            sqlite.clone(),
            policy,
        );
    }

    let client = crate::config::build_proxied_client(config.server.proxy.as_deref())
        .unwrap_or_else(|err| {
            log::warn!("invalid proxy config, ignoring it: {err}");
            Client::new()
        });
    let accounts = AccountResolver::with_default_store(client.clone());
    let copilot = Arc::new(CopilotSessions::new(client.clone()));
    let hosts = Arc::new(ManagedHosts::with_store(Arc::new(PluginAuthStore::new(
        plugin_auth_path(),
    ))));
    let secrets_path = crate::config::secrets_path();
    let data_dir = secrets_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let gateway_url = format!("http://{}:{}", config.server.host, config.server.port);
    let agents = Arc::new(AgentRegistry::new(
        data_dir,
        Arc::new(FileSecretStore::new(secrets_path)),
        gateway_url,
        config.server.api_key.clone(),
    ));
    agents.set_catalog(catalog_from_config(&config));
    Arc::new(AppState {
        config,
        client,
        accounts,
        copilot,
        copilot_allowances: Arc::new(CopilotAllowances::default()),
        usage_logger,
        request_logger,
        hosts,
        agents,
    })
}

/// Create a router with a pre-built `AppState`, enabling test injection of
/// custom HTTP clients (e.g., pointing at a local `wiremock` server).
///
/// The sign-in task registry is the process-wide one (magpie `signIns`),
/// seeded here with the server-level proxy so `run_with_shutdown` can
/// cancel every outstanding task on exit.
pub fn create_router_with_state(state: Arc<AppState>) -> Router {
    let runtime = legacy_runtime(&state);
    create_router_with_runtime(state, runtime)
}

fn legacy_runtime(state: &AppState) -> AdminRuntime {
    AdminRuntime {
        authority: ManagementAuthority::from_legacy_key(state.config.server.api_key.as_deref()),
        control_plane: None,
        web_auth: None,
        discoveries: std::sync::Arc::new(crate::model_discovery::DiscoveryCache::default()),
    }
}

/// Create a legacy router (fixed management authority) that also accepts
/// browser page sessions. Test seam for the managed gateway's web auth.
pub fn create_router_with_state_and_web_auth(
    state: Arc<AppState>,
    web_auth: Arc<WebAuth>,
) -> Router {
    let runtime = legacy_runtime(&state).with_web_auth(web_auth);
    create_router_with_runtime(state, runtime)
}

/// Create a router whose management surface is authoritative and backed by
/// the supplied control plane.
pub fn create_router_with_control_plane(
    state: Arc<AppState>,
    control_plane: Arc<ControlPlane>,
) -> Router {
    create_router_with_runtime(
        state,
        AdminRuntime::control_plane().with_control_plane(control_plane),
    )
}

/// Create a control-plane router that also accepts browser page sessions.
pub fn create_router_with_control_plane_and_web_auth(
    state: Arc<AppState>,
    control_plane: Arc<ControlPlane>,
    web_auth: Arc<WebAuth>,
) -> Router {
    create_router_with_runtime(
        state,
        AdminRuntime::control_plane()
            .with_control_plane(control_plane)
            .with_web_auth(web_auth),
    )
}

/// Build the managed router from a prepared, secret-resolved configuration.
pub fn create_router_from_prepared(prepared: crate::control_plane::PreparedServer) -> Router {
    let crate::control_plane::PreparedServer {
        config,
        control_plane,
        web_auth,
        ..
    } = prepared;
    create_router_with_control_plane_and_web_auth(build_app_state(config), control_plane, web_auth)
}

fn create_router_with_runtime(state: Arc<AppState>, runtime: AdminRuntime) -> Router {
    // The first router build of the process is process start for the status
    // uptime counter (see `handlers::data::note_start_time`).
    crate::handlers::data::note_start_time();
    let sign_ins = SignInTasks::init(
        state.config.server.proxy.clone(),
        Some(Arc::clone(&state.hosts)),
    );
    // Embedded when the frontend was built, `ANY_CONVERTER_WEB_DIR` when
    // set, otherwise 503 with build instructions.
    let web_ui = crate::web_ui::WebUi::discover();

    // The page session exchange is public by key, not bearer-guarded, so
    // it lives outside the management router. `/` lives here too: the
    // `/?k=` link needs `web_auth` and the SPA needs the `WebUi`
    // extension added on the outer router.
    let session = Router::new()
        .route("/", get(web_ui::handle_root))
        .route(
            "/v1/session",
            get(web_auth::get_session)
                .post(web_auth::post_session)
                .delete(web_auth::delete_session),
        )
        .layer(Extension(runtime.clone()));

    // Accounts, agents, usage, and the admin API are one management surface:
    // they share the management key and reject agent/inference keys.
    let management = Router::new()
        .route("/v1/usage", get(handle_usage_api))
        .route("/v1/admin/logs", get(handle_admin_logs))
        .route("/v1/admin/usage", get(handle_admin_usage))
        .route("/v1/admin/status", get(handle_admin_status))
        .route("/v1/admin/convert", post(handle_admin_convert))
        .route("/v1/accounts", get(handle_accounts))
        .route("/v1/accounts/quotas", get(handle_account_quotas))
        .route("/v1/accounts/plugins", get(handle_account_plugins))
        .route(
            "/v1/accounts/plugins/install",
            post(handle_plugin_install),
        )
        .route(
            "/v1/accounts/plugins/uninstall",
            post(handle_plugin_uninstall),
        )
        .route("/v1/accounts/{id}/models", get(handle_account_models))
        .route("/v1/accounts/order", post(handle_account_order))
        .route("/v1/accounts/{id}/adopt", post(handle_account_adopt))
        .route("/v1/accounts/{id}/release", post(handle_account_release))
        .route("/v1/accounts/signin", post(handle_account_signin))
        .route(
            "/v1/accounts/signin/{id}",
            get(handle_account_signin_status),
        )
        .route(
            "/v1/accounts/signin/{id}/submit",
            post(handle_account_signin_submit),
        )
        .route(
            "/v1/accounts/signin/{id}/cancel",
            post(handle_account_signin_cancel),
        )
        .route("/v1/accounts/{id}/remove", post(handle_account_remove))
        .route("/v1/agents", get(handle_agents_list))
        .route("/v1/agents/{id}/connect", post(handle_agent_connect))
        .route("/v1/agents/{id}/disconnect", post(handle_agent_disconnect))
        .route("/v1/agents/{id}/sync", post(handle_agent_sync))
        .route("/v1/agents/{id}/key/rotate", post(handle_agent_rotate_key))
        .route("/v1/agents/{id}/keys", get(handle_agent_keys))
        .route(
            "/v1/admin/config",
            get(admin::get_config).patch(admin::patch_config),
        )
        .route("/v1/admin/server/status", get(admin::get_status))
        .route(
            "/v1/admin/providers/discover",
            post(admin::discover_provider),
        )
        .route(
            "/v1/admin/providers/{name}/models",
            get(admin::get_provider_models),
        )
        .route("/v1/admin/catalog", get(admin::get_catalog))
        .route(
            "/v1/admin/secrets/provider/{provider}",
            put(admin::put_provider_secret).delete(admin::delete_provider_secret),
        )
        .route(
            "/v1/admin/secrets/inference",
            put(admin::put_inference_secret).delete(admin::delete_inference_secret),
        )
        .route(
            "/v1/admin/secrets/management/rotate",
            post(admin::rotate_management_secret),
        )
        .layer(middleware::from_fn(admin::require_management))
        .layer(Extension(runtime));

    let inference = Router::new()
        // With /v1/ prefix (standard)
        .route("/v1/chat/completions", post(handle_openai_chat))
        .route("/v1/messages", post(handle_claude))
        .route("/v1/responses", post(handle_responses))
        .route("/v1/messages/count_tokens", post(handle_count_tokens))
        // Without /v1/ prefix (Codex CLI and some clients)
        .route("/chat/completions", post(handle_openai_chat))
        .route("/messages", post(handle_claude))
        .route("/responses", post(handle_responses))
        .route("/messages/count_tokens", post(handle_count_tokens))
        .route("/v1beta/models/{*model_action}", post(handle_gemini));

    Router::new()
        // a new route here must be added to ROUTES in crates/server/tests/route_guard.rs
        .route("/health", get(handle_health))
        .route("/v1/models", get(handle_models))
        .route("/models", get(handle_models))
        .route("/v1/models/{model_id}", get(handle_model_retrieve))
        .route("/models/{model_id}", get(handle_model_retrieve))
        .merge(management)
        .merge(session)
        .merge(inference)
        .fallback(web_ui::handle_fallback)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)) // 10 MB
        .layer(Extension(sign_ins))
        .layer(Extension(web_ui))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use crate::config::{
        LoggingConfig, PassThroughHeadersConfig, ProviderConfig, RouteConfig, ServerSettings,
    };

    fn test_config() -> ServerConfig {
        ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![ProviderConfig {
                name: "openai".into(),
                format: Format::OpenAIChat,
                base_url: "https://api.openai.com".into(),
                api_key: "sk-test".into(),
                account: None,
                accounts: Vec::new(),
                api: None,
                wire: None,
                model_map: [("claude-sonnet-4".into(), "gpt-4.1".into())].into(),
                endpoints: Default::default(),
                auth: Default::default(),
                plugin: None,
                proxy: None,
            }],
            model_routes: vec![],
            routes: vec![
                RouteConfig {
                    client_format: Format::OpenAIChat,
                    provider: "openai".into(),
                },
                RouteConfig {
                    client_format: Format::Claude,
                    provider: "openai".into(),
                },
            ],
            model_metadata: std::collections::HashMap::new(),
            pricing: Default::default(),
            logging: LoggingConfig::default(),
        }
    }

    #[test]
    fn test_detect_format_openai_chat() {
        let info = detect_format_from_path("/v1/chat/completions").unwrap();
        assert_eq!(info.client_format, Format::OpenAIChat);
        assert!(!info.path_streaming);
    }

    #[test]
    fn test_detect_format_claude() {
        let info = detect_format_from_path("/v1/messages").unwrap();
        assert_eq!(info.client_format, Format::Claude);
    }

    #[test]
    fn test_detect_format_responses() {
        let info = detect_format_from_path("/v1/responses").unwrap();
        assert_eq!(info.client_format, Format::OpenAIResponses);
    }

    #[test]
    fn test_detect_format_gemini_stream() {
        let info =
            detect_format_from_path("/v1beta/models/gemini-pro:streamGenerateContent").unwrap();
        assert_eq!(info.client_format, Format::Gemini);
        assert!(info.path_streaming);
    }

    #[test]
    fn test_detect_format_gemini_non_stream() {
        let info = detect_format_from_path("/v1beta/models/gemini-pro:generateContent").unwrap();
        assert_eq!(info.client_format, Format::Gemini);
        assert!(!info.path_streaming);
    }

    #[test]
    fn test_detect_format_unknown_path() {
        assert!(detect_format_from_path("/unknown").is_none());
        assert!(detect_format_from_path("/health").is_none());
    }

    #[tokio::test]
    async fn test_health_check_returns_200() {
        let app = create_router(test_config());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_unknown_api_path_returns_404() {
        let app = create_router(test_config());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_unknown_non_api_path_serves_the_spa() {
        let app = create_router(test_config());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // The SPA fallback answers with the index (or the build-hint 503
        // when no UI is available), never 404.
        assert!(matches!(
            response.status(),
            StatusCode::OK | StatusCode::SERVICE_UNAVAILABLE
        ));
    }

    #[tokio::test]
    async fn test_models_endpoint_returns_model_list() {
        let app = create_router(test_config());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["object"], "list");
        assert!(!json["data"].as_array().unwrap().is_empty());
    }
}
