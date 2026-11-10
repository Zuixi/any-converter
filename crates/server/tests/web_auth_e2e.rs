//! Browser page-session integration tests: the cookie exchange, the
//! management middleware's cookie acceptance, and the loopback / same-origin
//! policy around it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use any_converter_core::convert::Format;
use any_converter_server::config::{
    LoggingConfig, ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy,
    ServerConfig, ServerSettings,
};
use any_converter_server::credentials::AccountResolver;
use any_converter_server::handlers::AppState;
use any_converter_server::router::{
    create_router_with_state, create_router_with_state_and_web_auth,
};
use any_converter_server::web_auth::WebAuth;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use reqwest::Client;
use tower::ServiceExt;

const HOST: &str = "127.0.0.1:1228";
const MANAGEMENT_KEY: &str = "sk-secret";
const PAGE_KEY: &str = "test-page-key-0001";

fn app_state() -> Arc<AppState> {
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: Some(MANAGEMENT_KEY.into()),
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        },
        providers: vec![ProviderConfig {
            name: "openai".into(),
            format: Format::OpenAIChat,
            base_url: "https://api.openai.com".into(),
            api_key: "upstream-key".into(),
            account: None,
            accounts: Vec::new(),
            api: None,
            wire: None,
            model_map: [("guard-model".into(), "upstream-model".into())].into(),
            endpoints: Default::default(),
            auth: Default::default(),
            plugin: None,
            proxy: None,
        }],
        model_routes: vec![ModelRouteConfig {
            pattern: "*".into(),
            provider: Some("openai".into()),
            providers: vec![],
            upstream_model: None,
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: Default::default(),
        pricing: Default::default(),
        logging: LoggingConfig::default(),
    };
    Arc::new(AppState {
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
        hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
            Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                std::env::temp_dir().join("any-converter-test-plugin-auth.json"),
            )),
        )),
    })
}

/// The managed-gateway test seam: legacy management key + page sessions.
fn session_app() -> Router {
    create_router_with_state_and_web_auth(
        app_state(),
        Arc::new(WebAuth::from_key(PAGE_KEY, true).unwrap()),
    )
}

/// The bearer-only legacy router (no web auth).
fn bearer_only_app() -> Router {
    create_router_with_state(app_state())
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut builder = Request::builder().uri(path).method(method);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    if !body.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

/// Exchange the page key for a live session cookie pair (`name=token`),
/// the way a browser would.
async fn unlock(app: &Router) -> String {
    let response = send(
        app,
        "POST",
        "/v1/session",
        &format!(r#"{{"key":"{PAGE_KEY}"}}"#),
        &[("host", HOST)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    cookie_pair(
        response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap(),
    )
}

/// `name=value` from a `Set-Cookie` value, attributes stripped.
fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().trim().to_owned()
}

#[tokio::test]
async fn post_session_with_correct_key_sets_a_port_scoped_cookie() {
    let app = session_app();
    let response = send(
        &app,
        "POST",
        "/v1/session",
        r#"{"key":"test-page-key-0001"}"#,
        &[("host", HOST)],
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.starts_with("any-converter-session-1228="));
    // The cookie carries a session token, never the page key itself.
    assert!(!cookie.contains(PAGE_KEY));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Max-Age=2592000"));
    assert!(cookie.contains("Priority=High"));
    // Session responses are never cacheable.
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}

#[tokio::test]
async fn post_session_with_wrong_key_is_401_without_a_cookie() {
    let app = session_app();
    let response = send(
        &app,
        "POST",
        "/v1/session",
        r#"{"key":"wrong-page-key-1234"}"#,
        &[("host", HOST)],
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn session_cookie_authenticates_management_routes() {
    let app = session_app();
    let session = unlock(&app).await;
    let response = send(
        &app,
        "GET",
        "/v1/admin/status",
        "",
        &[
            ("host", HOST),
            ("cookie", session.as_str()),
            ("sec-fetch-site", "same-origin"),
        ],
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn management_bearer_still_works() {
    let app = session_app();
    let response = send(
        &app,
        "GET",
        "/v1/admin/status",
        "",
        &[("authorization", "Bearer sk-secret")],
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn invalid_bearer_fails_closed_even_with_a_valid_cookie() {
    let app = session_app();
    let session = unlock(&app).await;
    let response = send(
        &app,
        "GET",
        "/v1/admin/status",
        "",
        &[
            ("host", HOST),
            ("cookie", session.as_str()),
            ("authorization", "Bearer wrong"),
        ],
    )
    .await;

    // The supplied bearer is authoritative: it fails closed (403) and the
    // valid cookie is never consulted.
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cookie_for_another_port_is_not_accepted() {
    let app = session_app();
    let response = send(
        &app,
        "GET",
        "/v1/admin/status",
        "",
        &[
            ("host", HOST),
            (
                "cookie",
                "any-converter-session-1229=0123456789abcdef0123456789abcdef",
            ),
        ],
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cookie_from_a_non_loopback_host_is_forbidden() {
    let app = session_app();
    let session = unlock(&app).await;
    let response = send(
        &app,
        "GET",
        "/v1/admin/status",
        "",
        &[("host", "192.168.1.5:1228"), ("cookie", session.as_str())],
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_site_fetch_metadata_is_forbidden() {
    let app = session_app();
    let session = unlock(&app).await;
    let response = send(
        &app,
        "GET",
        "/v1/admin/status",
        "",
        &[
            ("host", HOST),
            ("cookie", session.as_str()),
            ("sec-fetch-site", "cross-site"),
        ],
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_origin_mutation_is_forbidden() {
    let app = session_app();
    let session = unlock(&app).await;
    let response = send(
        &app,
        "POST",
        "/v1/admin/secrets/management/rotate",
        "",
        &[
            ("host", HOST),
            ("cookie", session.as_str()),
            ("origin", "http://evil.example"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // A matching Origin passes the guard; the handler then answers 503
    // because this router has no control plane — i.e. not 401/403.
    let response = send(
        &app,
        "POST",
        "/v1/admin/secrets/management/rotate",
        "",
        &[
            ("host", HOST),
            ("cookie", session.as_str()),
            ("origin", "http://127.0.0.1:1228"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn get_session_reports_the_cookie_state() {
    let app = session_app();
    let session = unlock(&app).await;
    let response = send(
        &app,
        "GET",
        "/v1/session",
        "",
        &[("host", HOST), ("cookie", session.as_str())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = send(&app, "GET", "/v1/session", "", &[("host", HOST)]).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_session_rejects_non_loopback_and_cross_origin_requests() {
    let app = session_app();
    let body = r#"{"key":"test-page-key-0001"}"#;

    let response = send(
        &app,
        "POST",
        "/v1/session",
        body,
        &[("host", "192.168.1.5:1228")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get(header::SET_COOKIE).is_none());

    let response = send(
        &app,
        "POST",
        "/v1/session",
        body,
        &[("host", HOST), ("origin", "http://evil.example")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn post_session_rejects_a_malformed_body() {
    let app = session_app();
    let response = send(
        &app,
        "POST",
        "/v1/session",
        r#"{"key":42}"#,
        &[("host", HOST)],
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn delete_session_clears_the_cookie() {
    let app = session_app();
    let response = send(&app, "DELETE", "/v1/session", "", &[("host", HOST)]).await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("any-converter-session-1228=;"));
    assert!(cookie.contains("Max-Age=0"));
}

#[tokio::test]
async fn session_routes_are_503_without_web_auth() {
    let app = bearer_only_app();
    let response = send(&app, "GET", "/v1/session", "", &[("host", HOST)]).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let response = send(
        &app,
        "POST",
        "/v1/session",
        r#"{"key":"test-page-key-0001"}"#,
        &[("host", HOST)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    // DELETE is unconditional: sign-out must work even without a key.
    let response = send(&app, "DELETE", "/v1/session", "", &[("host", HOST)]).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn the_page_key_itself_is_not_a_valid_cookie() {
    // The pre-token behavior (cookie value == key) is gone: a key disclosed
    // through some other channel cannot be replayed as a session cookie.
    let app = session_app();
    let response = send(
        &app,
        "GET",
        "/v1/admin/status",
        "",
        &[
            ("host", HOST),
            ("cookie", "any-converter-session-1228=test-page-key-0001"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_session_revokes_the_token() {
    let app = session_app();
    let session = unlock(&app).await;

    let response = send(
        &app,
        "DELETE",
        "/v1/session",
        "",
        &[("host", HOST), ("cookie", session.as_str())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // The same cookie no longer authenticates anything.
    let response = send(
        &app,
        "GET",
        "/v1/admin/status",
        "",
        &[("host", HOST), ("cookie", session.as_str())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn key_exchange_throttles_after_ten_failures() {
    let app = session_app();
    for attempt in 0..10 {
        let response = send(
            &app,
            "POST",
            "/v1/session",
            r#"{"key":"wrong-key-attempt-x"}"#,
            &[("host", HOST)],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "attempt {attempt}"
        );
    }
    let response = send(
        &app,
        "POST",
        "/v1/session",
        r#"{"key":"wrong-key-attempt-x"}"#,
        &[("host", HOST)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // A known-good key is never denial-of-serviced by bad guesses; it also
    // resets the failure window for later mistakes.
    let response = send(
        &app,
        "POST",
        "/v1/session",
        &format!(r#"{{"key":"{PAGE_KEY}"}}"#),
        &[("host", HOST)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = send(
        &app,
        "POST",
        "/v1/session",
        r#"{"key":"wrong-key-after-reset"}"#,
        &[("host", HOST)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
