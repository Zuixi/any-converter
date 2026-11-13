//! Structural route guard.
//!
//! axum cannot enumerate a `Router`'s routes at runtime, so this is an explicit
//! inventory: keep `ROUTES` in sync with `create_router_with_state`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    LoggingConfig, ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy,
    ServerConfig, ServerSettings,
};
use any_converter_server::credentials::AccountResolver;
use any_converter_server::handlers::AppState;
use any_converter_server::router::{create_router_with_state, detect_format_from_path};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use reqwest::Client;
use tower::ServiceExt;

/// Every route registered by `create_router_with_state`.
///
/// `(method, path, minimal body)` — GET routes carry an empty body.
const ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/", ""),
    ("GET", "/health", ""),
    ("GET", "/v1/models", ""),
    ("GET", "/models", ""),
    ("GET", "/v1/session", ""),
    ("POST", "/v1/session", r#"{"key":"x"}"#),
    ("DELETE", "/v1/session", ""),
    ("GET", "/v1/usage", ""),
    ("GET", "/v1/admin/logs", ""),
    ("GET", "/v1/admin/usage", ""),
    ("GET", "/v1/admin/status", ""),
    (
        "POST",
        "/v1/admin/convert",
        r#"{"input":"{}","from":"openai_chat","to":"claude","mode":"request"}"#,
    ),
    ("GET", "/v1/admin/config", ""),
    ("PATCH", "/v1/admin/config", "{}"),
    ("GET", "/v1/admin/server/status", ""),
    (
        "POST",
        "/v1/admin/providers/discover",
        r#"{"format":"openai_chat","base_url":"https://example.invalid","api_key":"sk-test"}"#,
    ),
    ("GET", "/v1/admin/providers/guard-provider/models", ""),
    ("GET", "/v1/admin/catalog", ""),
    (
        "PUT",
        "/v1/admin/secrets/provider/guard-provider",
        r#"{"api_key":"sk-test"}"#,
    ),
    ("DELETE", "/v1/admin/secrets/provider/guard-provider", ""),
    (
        "PUT",
        "/v1/admin/secrets/inference",
        r#"{"api_key":"sk-test"}"#,
    ),
    ("DELETE", "/v1/admin/secrets/inference", ""),
    ("POST", "/v1/admin/secrets/management/rotate", ""),
    ("GET", "/v1/accounts", ""),
    ("GET", "/v1/accounts/guard/models", ""),
    ("GET", "/v1/models/guard-model", ""),
    ("GET", "/models/guard-model", ""),
    ("GET", "/v1/accounts/quotas", ""),
    ("GET", "/v1/accounts/plugins", ""),
    (
        "POST",
        "/v1/accounts/order",
        r#"{"provider":"x","accounts":[]}"#,
    ),
    ("POST", "/v1/accounts/signin", r#"{"provider":"gemini"}"#),
    ("GET", "/v1/accounts/signin/guard", ""),
    (
        "POST",
        "/v1/accounts/signin/guard/submit",
        r#"{"input":"code"}"#,
    ),
    ("POST", "/v1/accounts/signin/guard/cancel", ""),
    ("POST", "/v1/accounts/guard/adopt", ""),
    ("POST", "/v1/accounts/guard/release", ""),
    ("POST", "/v1/accounts/guard/remove", ""),
    ("GET", "/v1/agents", ""),
    ("GET", "/v1/agents/claude-code/keys", ""),
    // Invalid body: reaches the handler (400) without touching a real home.
    ("POST", "/v1/agents/claude-code/connect", "not-json"),
    ("POST", "/v1/agents/claude-code/disconnect", ""),
    ("POST", "/v1/agents/claude-code/sync", ""),
    ("POST", "/v1/agents/claude-code/key/rotate", ""),
    (
        "POST",
        "/v1/chat/completions",
        r#"{"model":"guard-model","messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/chat/completions",
        r#"{"model":"guard-model","messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/v1/messages",
        r#"{"model":"guard-model","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/messages",
        r#"{"model":"guard-model","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/v1/responses",
        r#"{"model":"guard-model","input":"hi"}"#,
    ),
    (
        "POST",
        "/responses",
        r#"{"model":"guard-model","input":"hi"}"#,
    ),
    (
        "POST",
        "/v1/messages/count_tokens",
        r#"{"model":"guard-model","messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/messages/count_tokens",
        r#"{"model":"guard-model","messages":[{"role":"user","content":"hi"}]}"#,
    ),
    (
        "POST",
        "/v1beta/models/m1:generateContent",
        r#"{"contents":[{"parts":[{"text":"hi"}]}]}"#,
    ),
];

fn make_app(base_url: &str, api_key: Option<&str>) -> Router {
    make_app_with_log_dir(base_url, api_key, None)
}

fn make_app_with_log_dir(base_url: &str, api_key: Option<&str>, log_dir: Option<&str>) -> Router {
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: api_key.map(String::from),
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        },
        providers: vec![ProviderConfig {
            name: "openai".into(),
            format: Format::OpenAIChat,
            base_url: base_url.into(),
            api_key: "upstream-key".into(),
            account: None,
            accounts: Vec::new(),
            api: None,
            wire: None,
            // Literal key so `/models/{model_id}` resolves instead of 404.
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
        logging: LoggingConfig {
            dir: log_dir.map(String::from),
            ..Default::default()
        },
    };
    create_router_with_state(Arc::new(AppState {
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
    }))
}

async fn spawn_upstream(hits: Arc<AtomicUsize>) -> String {
    let router = Router::new().fallback(move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            (StatusCode::INTERNAL_SERVER_ERROR, "upstream down")
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn send(app: Router, method: &str, path: &str, body: &str, auth: Option<&str>) -> StatusCode {
    send_response(app, method, path, body, auth).await.status()
}

async fn send_response(
    app: Router,
    method: &str,
    path: &str,
    body: &str,
    auth: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .uri(path)
        .method(method)
        .header("Content-Type", "application/json");
    if let Some(auth) = auth {
        builder = builder.header("Authorization", auth);
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn send_json_with_auth(
    app: Router,
    path: &str,
    auth: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let response = send_response(app, "GET", path, "", auth).await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn admin_mutations_fail_closed_before_control_plane_load() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let app = make_app(&base, None);

    for (method, path, body) in ROUTES {
        if !matches!(*method, "POST" | "PUT" | "DELETE" | "PATCH") || !path.starts_with("/v1/admin")
        {
            continue;
        }
        let status = send(app.clone(), method, path, body, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} must fail closed before the handler"
        );
    }
}

#[tokio::test]
async fn every_route_answers_not_404() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let app = make_app(&base, None);

    for (method, path, body) in ROUTES {
        let status = send(app.clone(), method, path, body, None).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} fell through to the 404 fallback"
        );
    }
}

/// `POST /v1/session` is deliberately public: the browser sends the page
/// key in the body (it has no bearer to send), and the handler only sets a
/// cookie when the key matches. This route_guard app has no web auth, so
/// the route answers 503 rather than 401.
const PUBLIC_POST_PATHS: &[&str] = &["/v1/session"];

#[tokio::test]
async fn post_routes_require_auth_and_never_reach_upstream() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits.clone()).await;
    let app = make_app(&base, Some("secret"));

    for (method, path, body) in ROUTES {
        if *method != "POST" || PUBLIC_POST_PATHS.contains(path) {
            continue;
        }
        let status = send(app.clone(), method, path, body, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} accepted a request without Authorization"
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "unauthenticated POST reached the upstream"
    );

    // Public GET endpoints stay reachable when an api_key is configured.
    for path in ["/health", "/v1/models"] {
        let status = send(app.clone(), "GET", path, "", None).await;
        assert_eq!(status, StatusCode::OK, "GET {path} should be public");
    }
}

#[tokio::test]
async fn usage_api_is_503_without_a_log_directory() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let app = make_app(&base, Some("sk-secret"));

    let response = send_response(app, "GET", "/v1/usage", "", Some("Bearer sk-secret")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("logging"),
        "the body should name logging: {json}"
    );
}

#[tokio::test]
async fn usage_api_answers_an_empty_summary_for_a_fresh_log_directory() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let (status, json) = send_json_with_auth(app, "/v1/usage", Some("Bearer sk-secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["totals"]["calls"], 0);
    assert_eq!(json["totals"]["input_tokens"], 0);
    assert_eq!(json["series"].as_array().unwrap().len(), 0);
    assert_eq!(json["by_provider"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn usage_api_requires_the_client_key() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let status = send(app.clone(), "GET", "/v1/usage", "", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The configured key gets in, so the guard is not simply refusing everything.
    let status = send(app, "GET", "/v1/usage", "", Some("Bearer sk-secret")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn usage_api_rejects_an_unknown_preset() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let status = send(
        app,
        "GET",
        "/v1/usage?preset=nonsense",
        "",
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn usage_api_accepts_granularity_and_split_by() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let (status, json) = send_json_with_auth(
        app,
        "/v1/usage?granularity=week&split_by=provider",
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["granularity"], "week");
}

#[tokio::test]
async fn admin_data_routes_require_the_management_key() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    for (method, path, body) in [
        ("GET", "/v1/admin/logs", ""),
        ("GET", "/v1/admin/usage", ""),
        ("GET", "/v1/admin/status", ""),
        (
            "POST",
            "/v1/admin/convert",
            r#"{"input":"{\"model\":\"m\",\"max_tokens\":16,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}","from":"openai_chat","to":"claude","mode":"request"}"#,
        ),
    ] {
        let status = send(app.clone(), method, path, body, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} without auth"
        );

        let status = send(app.clone(), method, path, body, Some("Bearer wrong")).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {path} with a wrong key"
        );

        let status = send(app.clone(), method, path, body, Some("Bearer sk-secret")).await;
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "{method} {path} with the management key: {status}"
        );
    }
}

#[tokio::test]
async fn admin_logs_answers_empty_records_for_a_fresh_log_directory() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let (status, json) =
        send_json_with_auth(app.clone(), "/v1/admin/logs", Some("Bearer sk-secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["records"].as_array().unwrap().len(), 0);

    let status = send(
        app,
        "GET",
        "/v1/admin/logs?limit=abc",
        "",
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_logs_is_503_without_a_log_directory() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let app = make_app(&base, Some("sk-secret"));

    let response = send_response(app, "GET", "/v1/admin/logs", "", Some("Bearer sk-secret")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["code"], "logging_not_configured");
}

#[tokio::test]
async fn admin_logs_falls_back_to_the_jsonl_mirror() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let record = |id: &str, timestamp: &str| {
        format!(
            r#"{{"request_id":"{id}","timestamp":"{timestamp}","client_format":"openai_chat","provider":"openai","client_model":"m","upstream_model":"m","streaming":false,"method":"POST","path":"/v1/chat/completions","request_body":null,"upstream_request_body":null,"response_status":200,"response_body":{{"text":"ok"}},"latency_ms":5,"usage":{{"input_tokens":1,"output_tokens":1}},"truncated":false}}"#
        )
    };
    std::fs::write(
        dir.path().join("requests.2026-01-01.jsonl"),
        format!(
            "{}\n{}\nnot-json\n",
            record("older", "2026-01-01T00:00:00Z"),
            record("newer", "2026-01-02T00:00:00Z"),
        ),
    )
    .unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let (status, json) = send_json_with_auth(app, "/v1/admin/logs", Some("Bearer sk-secret")).await;
    assert_eq!(status, StatusCode::OK);
    let records = json["records"].as_array().unwrap();
    assert_eq!(records.len(), 2, "the malformed line is skipped: {json}");
    assert_eq!(records[0]["timestamp"], "2026-01-02T00:00:00Z");
    assert_eq!(records[1]["timestamp"], "2026-01-01T00:00:00Z");
}

#[tokio::test]
async fn admin_usage_answers_empty_shapes_for_a_fresh_log_directory() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let (status, json) =
        send_json_with_auth(app.clone(), "/v1/admin/usage", Some("Bearer sk-secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["totals"]["calls"], 0);
    assert_eq!(json["summary"]["series"].as_array().unwrap().len(), 0);

    let (status, json) = send_json_with_auth(
        app.clone(),
        "/v1/admin/usage?ledger=1",
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["page"]["rows"].as_array().unwrap().len(), 0);
    assert_eq!(json["page"]["total"], 0);

    let (status, json) = send_json_with_auth(
        app.clone(),
        "/v1/admin/usage?heatmap=1",
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["heatmap"], serde_json::Value::Null);

    for query in [
        "/v1/admin/usage?from=bad&to=worse",
        "/v1/admin/usage?preset=nonsense",
        "/v1/admin/usage?ledger=1&cursor=abc",
        "/v1/admin/usage?ledger=1&limit=-3",
    ] {
        let status = send(app.clone(), "GET", query, "", Some("Bearer sk-secret")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
    }
}

#[tokio::test]
async fn admin_usage_csv_has_the_exact_header() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let response = send_response(
        app,
        "GET",
        "/v1/admin/usage?format=csv",
        "",
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.starts_with("text/csv"), "{content_type}");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(body.to_vec()).unwrap(),
        "timestamp,provider,client_model,upstream_model,served_model,agent,account,\
         session_id,status,streaming,latency_ms,ttft_ms,input_tokens,output_tokens,\
         cache_read_tokens,cache_write_tokens,reasoning_tokens,cost_usd",
    );
}

#[tokio::test]
async fn admin_status_reports_the_gateway_process() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let (status, json) =
        send_json_with_auth(app, "/v1/admin/status", Some("Bearer sk-secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["health"]["status"], "ok");
    assert!(json["recentErrors"].is_array());
    assert!(!json["version"].as_str().unwrap().is_empty());
    assert!(json["pid"].is_number());
    assert_eq!(json["disk"]["used_bytes"], 0);
    assert_eq!(json["disk"]["max_bytes"], 500 * 1024 * 1024);
    assert_eq!(json["address"], "127.0.0.1:8080");
    assert!(json["uptime_seconds"].as_u64().is_some());
}

#[tokio::test]
async fn admin_status_answers_200_without_a_log_directory() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let app = make_app(&base, Some("sk-secret"));

    let (status, json) =
        send_json_with_auth(app, "/v1/admin/status", Some("Bearer sk-secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["disk"]["max_bytes"], serde_json::Value::Null);
    assert_eq!(json["disk"]["percent"], serde_json::Value::Null);
}

#[tokio::test]
async fn admin_convert_converts_an_openai_chat_body() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn_upstream(hits).await;
    let dir = tempfile::tempdir().unwrap();
    let app = make_app_with_log_dir(&base, Some("sk-secret"), Some(dir.path().to_str().unwrap()));

    let body = r#"{"input":"{\"model\":\"m\",\"max_tokens\":16,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}","from":"openai_chat","to":"claude","mode":"request"}"#;
    let response = send_response(
        app.clone(),
        "POST",
        "/v1/admin/convert",
        body,
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(!json["output"].as_str().unwrap().is_empty());
    assert!(json.get("error").is_none());

    let unknown_mode = r#"{"input":"{}","from":"openai_chat","to":"claude","mode":"nonsense"}"#;
    let status = send(
        app.clone(),
        "POST",
        "/v1/admin/convert",
        unknown_mode,
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let status = send(
        app,
        "POST",
        "/v1/admin/convert",
        "not-json",
        Some("Bearer sk-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn detect_format_matches_client_paths() {
    let expected = [
        ("/v1/chat/completions", Some(Format::OpenAIChat)),
        ("/v1/messages", Some(Format::Claude)),
        ("/v1/responses", Some(Format::OpenAIResponses)),
        ("/v1beta/models/x:generateContent", Some(Format::Gemini)),
        (
            "/v1beta/models/x:streamGenerateContent",
            Some(Format::Gemini),
        ),
        ("/health", None),
        ("/v1/models", None),
        ("/no/such/route", None),
    ];

    for (path, format) in expected {
        assert_eq!(
            detect_format_from_path(path).map(|info| info.client_format),
            format,
            "format drift for {path}"
        );
    }
}
