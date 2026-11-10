//! UI serving integration tests: the `/?k=` page-key link and the SPA
//! fallback for non-API paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use any_converter_core::convert::Format;
use any_converter_server::config::{
    LoggingConfig, ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy,
    ServerConfig, ServerSettings,
};
use any_converter_server::credentials::AccountResolver;
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state_and_web_auth;
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

async fn send(app: Router, path: &str, headers: &[(&str, &str)]) -> axum::response::Response {
    let mut builder = Request::builder().uri(path).method("GET");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn key_link_sets_the_session_cookie_and_redirects() {
    let app = session_app();
    let response = send(app, "/?k=test-page-key-0001", &[("host", HOST)]).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap(),
        "/"
    );
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    // The cookie carries a minted session token, never the page key.
    assert!(
        cookie.starts_with("any-converter-session-1228="),
        "{cookie}"
    );
    assert!(!cookie.contains("test-page-key-0001"), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert_eq!(
        response.headers().get(header::REFERRER_POLICY).unwrap(),
        "no-referrer"
    );
}

#[tokio::test]
async fn key_link_preserves_other_query_parameters() {
    let app = session_app();
    let response = send(app, "/?k=test-page-key-0001&x=1&y=2", &[("host", HOST)]).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap(),
        "/?x=1&y=2"
    );
}

#[tokio::test]
async fn wrong_key_link_is_401_without_a_cookie() {
    let app = session_app();
    let response = send(app, "/?k=wrong-page-key-1234", &[("host", HOST)]).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    assert_eq!(body_text(response).await, "invalid web key");
}

#[tokio::test]
async fn key_link_shares_throttling_without_locking_out_the_right_key() {
    let app = session_app();
    for attempt in 0..10 {
        let response = send(app.clone(), "/?k=wrong-page-key-1234", &[("host", HOST)]).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "attempt {attempt}"
        );
    }
    let response = send(app.clone(), "/?k=wrong-page-key-1234", &[("host", HOST)]).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    let response = send(app.clone(), "/?k=test-page-key-0001", &[("host", HOST)]).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let response = send(app, "/?k=wrong-page-key-1234", &[("host", HOST)]).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn key_link_rejects_cross_site_fetch_metadata() {
    let app = session_app();
    let response = send(
        app,
        "/?k=test-page-key-0001",
        &[("host", HOST), ("sec-fetch-site", "cross-site")],
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn key_link_rejects_a_non_loopback_host() {
    let app = session_app();
    let response = send(
        app,
        "/?k=test-page-key-0001",
        &[("host", "192.168.1.5:1228")],
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn unknown_api_path_stays_404_plain_text() {
    let app = session_app();
    let response = send(app, "/v1/nope", &[]).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        !response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| {
                value
                    .to_str()
                    .is_ok_and(|text| text.starts_with("text/html"))
            })
    );
    let text = body_text(response).await;
    assert!(
        !text.contains("<html"),
        "unknown API paths must never receive HTML: {text}"
    );
}

#[tokio::test]
async fn missing_asset_is_404_not_index() {
    let app = session_app();
    let response = send(app, "/assets/nope.js", &[]).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let text = body_text(response).await;
    assert!(
        !text.contains("<html"),
        "a missing asset must not fall back to the index: {text}"
    );
}

#[tokio::test]
async fn root_serves_or_explains() {
    let app = session_app();
    let response = send(app, "/", &[]).await;
    let status = response.status();
    let headers = response.headers().clone();
    let text = body_text(response).await;

    match status {
        StatusCode::OK => {
            assert_eq!(
                headers.get(header::CONTENT_TYPE).unwrap().to_str().unwrap(),
                "text/html; charset=utf-8"
            );
            assert_eq!(
                headers
                    .get(header::CACHE_CONTROL)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "no-cache"
            );
            assert!(
                headers
                    .get(header::CONTENT_SECURITY_POLICY)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("default-src 'self'")
            );
            assert!(text.contains("<html"), "{text}");
        }
        StatusCode::SERVICE_UNAVAILABLE => {
            assert!(text.contains("UI assets are not built"), "{text}");
        }
        other => panic!("GET / must answer 200 or 503, got {other}"),
    }
}
