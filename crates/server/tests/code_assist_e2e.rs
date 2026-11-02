//! End-to-end tests for the Gemini Code Assist wire ("accounts as first-class
//! citizens"): OAuth bearer signing, `{"model","request"}` envelope wrapping,
//! `{"response":...}` unwrapping (non-streaming and SSE), token refresh with
//! persistence, and the account-error surface — all against a fake Code
//! Assist upstream.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings,
};
use any_converter_server::credentials::{
    AccountResolver, Credential, FileSecretStore, SecretStore,
};
use any_converter_server::handlers::AppState;
use any_converter_server::lockfile::{FileLock, LockError};
use any_converter_server::router::create_router_with_state;
use axum::body::Body;
use axum::extract::Request as AxumRequest;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Router, extract::State as AxumState, routing::post};
use reqwest::Client;
use serde_json::{Value, json};
use tower::ServiceExt;

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ── Fake Code Assist upstream ────────────────────────────────────────────

/// Everything the fake upstream observed, shared with the test.
#[derive(Default)]
struct FakeUpstream {
    generate_path: Mutex<Option<String>>,
    generate_auth: Mutex<Option<String>>,
    generate_body: Mutex<Option<Value>>,
    token_calls: Mutex<usize>,
    token_body: Mutex<Option<String>>,
    /// Queued 401 responses: one body per upcoming generate request.
    generate_401_bodies: Mutex<Vec<String>>,
    /// Number of generate requests received.
    generate_calls: Mutex<usize>,
    /// How long the token endpoint sleeps before answering.
    token_delay_ms: Mutex<u64>,
}

impl FakeUpstream {
    fn generate_path(&self) -> Option<String> {
        self.generate_path.lock().unwrap().clone()
    }

    fn generate_auth(&self) -> Option<String> {
        self.generate_auth.lock().unwrap().clone()
    }

    fn generate_body(&self) -> Option<Value> {
        self.generate_body.lock().unwrap().clone()
    }

    fn token_calls(&self) -> usize {
        *self.token_calls.lock().unwrap()
    }

    fn token_body(&self) -> Option<String> {
        self.token_body.lock().unwrap().clone()
    }

    fn generate_calls(&self) -> usize {
        *self.generate_calls.lock().unwrap()
    }

    fn push_generate_401(&self, body: &str) {
        self.generate_401_bodies
            .lock()
            .unwrap()
            .push(body.to_string());
    }

    fn set_token_delay(&self, ms: u64) {
        *self.token_delay_ms.lock().unwrap() = ms;
    }
}

/// The non-streaming Code Assist endpoint: record the request, answer with
/// an enveloped `GenerateContentResponse`. When a 401 body is queued, fail
/// the request with it instead.
async fn generate_content(
    AxumState(state): AxumState<Arc<FakeUpstream>>,
    req: AxumRequest,
) -> Response {
    let code_assist = req.uri().path().starts_with("/v1internal");
    let fail_body = state.generate_401_bodies.lock().unwrap().pop();
    *state.generate_calls.lock().unwrap() += 1;
    record_generate_request(&state, req).await;
    if let Some(body) = fail_body {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::from_str::<Value>(&body).unwrap_or(Value::Null)),
        )
            .into_response();
    }
    let candidates = json!([{
        "content": {"parts": [{"text": "hi"}], "role": "model"}
    }]);
    // The Code Assist wire wraps the payload in a response envelope; the
    // plain Gemini path serves the bare body.
    if code_assist {
        axum::Json(json!({ "response": { "candidates": candidates } })).into_response()
    } else {
        axum::Json(json!({ "candidates": candidates })).into_response()
    }
}

/// The streaming Code Assist endpoint: same recording, then two enveloped SSE
/// events (a text delta and a finishReason chunk).
async fn stream_generate_content(
    AxumState(state): AxumState<Arc<FakeUpstream>>,
    req: AxumRequest,
) -> Response {
    record_generate_request(&state, req).await;
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"he\"}]}}]}}\n\n\
             data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}}\n\n",
        ))
        .unwrap()
}

async fn record_generate_request(state: &FakeUpstream, req: AxumRequest) {
    *state.generate_path.lock().unwrap() = Some(req.uri().path().to_string());
    *state.generate_auth.lock().unwrap() = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .unwrap();
    *state.generate_body.lock().unwrap() = serde_json::from_slice(&body).ok().or(Some(Value::Null));
}

/// Fake Google OAuth token endpoint: hands out `tok-2` on every refresh.
async fn token_endpoint(
    AxumState(state): AxumState<Arc<FakeUpstream>>,
    body: axum::body::Bytes,
) -> Response {
    let delay = *state.token_delay_ms.lock().unwrap();
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
    *state.token_calls.lock().unwrap() += 1;
    *state.token_body.lock().unwrap() = Some(String::from_utf8_lossy(&body).into_owned());
    axum::Json(json!({"access_token": "tok-2", "expires_in": 3600})).into_response()
}

/// Spawn the fake upstream on an ephemeral port; returns its base URL.
async fn spawn_upstream() -> (String, Arc<FakeUpstream>) {
    let state = Arc::new(FakeUpstream::default());
    let router = Router::new()
        .route("/v1internal:generateContent", post(generate_content))
        .route(
            "/v1internal:streamGenerateContent",
            post(stream_generate_content),
        )
        // Plain Gemini path for providers without the Code Assist wire.
        .route("/v1beta/models/{*rest}", post(generate_content))
        .route("/token", post(token_endpoint))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), state)
}

// ── Test scaffolding ──────────────────────────────────────────────────────

fn code_assist_provider(
    name: &str,
    base: &str,
    format: Format,
    account: Option<&str>,
) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format,
        base_url: base.into(),
        api_key: String::new(),
        account: account.map(String::from),
        accounts: Vec::new(),
        api: None,
        wire: None,
        model_map: Default::default(),
        endpoints: Default::default(),
        auth: Default::default(),
        plugin: None,
        proxy: None,
    }
}

fn server_config(provider: ProviderConfig) -> ServerConfig {
    ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        },
        providers: vec![provider],
        model_routes: vec![ModelRouteConfig {
            pattern: "gemini-oauth".into(),
            provider: Some("gemini-oauth".into()),
            providers: vec![],
            upstream_model: None,
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: Default::default(),
        logging: Default::default(),
    }
}

fn oauth_credential(access_token: &str, expires_at_unix: i64) -> Credential {
    Credential::OAuth {
        provider: "gemini".into(),
        account: "me@example.com".into(),
        access_token: access_token.into(),
        refresh_token: Some("ref-1".into()),
        expires_at_unix: Some(expires_at_unix),
    }
}

/// A temp secrets.json plus the AppState wired over it.
struct TestEnv {
    _dir: tempfile::TempDir,
    app: Router,
    accounts: Arc<AccountResolver>,
    upstream: Arc<FakeUpstream>,
    secrets_path: std::path::PathBuf,
}

impl TestEnv {
    async fn build(format: Format, account: Option<&str>, expires_at_unix: Option<i64>) -> Self {
        let (base, upstream) = spawn_upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let store = FileSecretStore::new(secrets_path.clone());
        if let Some(expiry) = expires_at_unix {
            store
                .put("gemini:me@example.com", &oauth_credential("tok-1", expiry))
                .unwrap();
        }

        let client = Client::builder().no_proxy().build().unwrap();
        // Refreshes go to the fake service's `/token` endpoint.
        let accounts = Arc::new(AccountResolver::with_token_url(
            store,
            client.clone(),
            format!("{base}/token"),
        ));
        let state = Arc::new(AppState {
            config: server_config(code_assist_provider("gemini-oauth", &base, format, account)),
            client,
            accounts: Arc::clone(&accounts),
            usage_logger: None,
            request_logger: None,
            copilot: Arc::new(
                any_converter_server::credentials::copilot::CopilotSessions::new(Client::new()),
            ),
            hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
                Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                    std::env::temp_dir().join("any-converter-test-plugin-auth.json"),
                )),
            )),
        });
        let app = create_router_with_state(state);

        Self {
            _dir: dir,
            app,
            accounts,
            upstream,
            secrets_path,
        }
    }

    async fn chat(&self, body: Value) -> (StatusCode, String) {
        chat_via(&self.app, body).await
    }

    fn revoked_path(&self) -> std::path::PathBuf {
        self._dir.path().join("revoked.json")
    }

    fn stored_oauth(&self) -> Value {
        let raw = std::fs::read_to_string(&self.secrets_path).unwrap();
        let map: Value = serde_json::from_str(&raw).unwrap();
        map["gemini:me@example.com"].clone()
    }
}

fn chat_body(stream: bool) -> Value {
    json!({
        "model": "gemini-oauth",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": stream,
    })
}

/// Send one chat request to `app` and return (status, body).
async fn chat_via(app: &Router, body: Value) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn non_streaming_gemini_provider_uses_oauth_and_envelope() {
    let env = TestEnv::build(
        Format::Gemini,
        Some("gemini:me@example.com"),
        Some(unix_now() + 3600),
    )
    .await;

    let (status, body) = env.chat(chat_body(false)).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["choices"][0]["message"]["content"], "hi");

    // The upstream saw the Code Assist path, OAuth bearer auth (api_key was
    // empty), and the two-level `{"model", "request"}` envelope.
    assert_eq!(
        env.upstream.generate_path().as_deref(),
        Some("/v1internal:generateContent")
    );
    assert_eq!(
        env.upstream.generate_auth().as_deref(),
        Some("Bearer tok-1")
    );
    let sent = env.upstream.generate_body().unwrap();
    assert_eq!(sent["model"], "gemini-oauth");
    assert!(
        sent["request"]["contents"].is_array(),
        "envelope must carry request.contents: {sent}"
    );
    assert!(
        sent.get("contents").is_none(),
        "no bare gemini body on the wire"
    );
}

#[tokio::test]
async fn non_streaming_openai_chat_provider_also_rides_code_assist() {
    // The provider declares openai_chat but the Code Assist wire converts to
    // Gemini before wrapping; the response still comes back in OpenAI shape.
    let env = TestEnv::build(
        Format::OpenAIChat,
        Some("gemini:me@example.com"),
        Some(unix_now() + 3600),
    )
    .await;

    let (status, body) = env.chat(chat_body(false)).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["choices"][0]["message"]["content"], "hi");

    let sent = env.upstream.generate_body().unwrap();
    assert_eq!(sent["model"], "gemini-oauth");
    assert!(sent["request"]["contents"].is_array(), "sent: {sent}");
}

#[tokio::test]
async fn streaming_unwraps_sse_response_envelope() {
    let env = TestEnv::build(
        Format::Gemini,
        Some("gemini:me@example.com"),
        Some(unix_now() + 3600),
    )
    .await;

    let (status, body) = env.chat(chat_body(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("\"he\""),
        "client SSE must carry the unwrapped delta: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "stream must end with [DONE]: {body}"
    );

    assert_eq!(
        env.upstream.generate_path().as_deref(),
        Some("/v1internal:streamGenerateContent")
    );
}

#[tokio::test]
async fn expired_token_refreshes_and_persists() {
    let env = TestEnv::build(
        Format::Gemini,
        Some("gemini:me@example.com"),
        Some(unix_now() - 100),
    )
    .await;

    let (status, body) = env.chat(chat_body(false)).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");

    // The token endpoint was hit first, with a refresh_token grant...
    assert_eq!(env.upstream.token_calls(), 1);
    let token_body = env.upstream.token_body().unwrap();
    assert!(
        token_body.contains("grant_type=refresh_token"),
        "{token_body}"
    );
    assert!(token_body.contains("refresh_token=ref-1"), "{token_body}");
    assert!(token_body.contains("client_id="), "{token_body}");

    // ...and the refreshed token signed the upstream request.
    assert_eq!(
        env.upstream.generate_auth().as_deref(),
        Some("Bearer tok-2")
    );

    // The refreshed credential was written back to the store.
    let stored = env.stored_oauth();
    assert_eq!(stored["access_token"], "tok-2");
    assert!(
        stored["expires_at_unix"].as_i64().unwrap() > unix_now(),
        "expiry must be in the future: {stored}"
    );

    // A second request rides the in-memory cache: no extra refresh.
    let (status, _) = env.chat(chat_body(false)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(env.upstream.token_calls(), 1);
}

#[tokio::test]
async fn unknown_account_returns_502_account_error() {
    // Account configured but never signed in: the secret store has no entry.
    let env = TestEnv::build(Format::Gemini, Some("gemini:nobody@example.com"), None).await;

    let (status, body) = env.chat(chat_body(false)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["error"]["type"], "account_error");
    assert!(
        parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not signed in"),
        "message: {body}"
    );
    // The upstream never saw the request.
    assert!(env.upstream.generate_path().is_none());
}

// ── 401 semantics (S6) ──────────────────────────────────────────────────

#[tokio::test]
async fn expired_401_refreshes_and_retries_once() {
    let env = TestEnv::build(
        Format::Gemini,
        Some("gemini:me@example.com"),
        Some(unix_now() + 3600),
    )
    .await;
    env.upstream
        .push_generate_401(r#"{"error":{"message":"token_expired"}}"#);

    let (status, body) = env.chat(chat_body(false)).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["choices"][0]["message"]["content"], "hi");

    // The token endpoint was hit exactly once (the Expired refresh)...
    assert_eq!(env.upstream.token_calls(), 1);
    // ...the upstream was hit exactly twice (401 + exactly one retry)...
    assert_eq!(env.upstream.generate_calls(), 2, "exactly one retry");
    // ...and the retried request carried the refreshed token.
    assert_eq!(
        env.upstream.generate_auth().as_deref(),
        Some("Bearer tok-2"),
        "the retry must use the refreshed token"
    );
    // The refreshed credential was persisted.
    let stored = env.stored_oauth();
    assert_eq!(stored["access_token"], "tok-2");
}

#[tokio::test]
async fn revoked_401_marks_account_and_fails_over() {
    let (revoked_base, revoked_upstream) = spawn_upstream().await;
    let (healthy_base, healthy_upstream) = spawn_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let secrets_path = dir.path().join("secrets.json");
    let store = FileSecretStore::new(secrets_path);
    store
        .put(
            "gemini:me@example.com",
            &oauth_credential("tok-1", unix_now() + 3600),
        )
        .unwrap();

    let client = Client::builder().no_proxy().build().unwrap();
    let accounts = Arc::new(AccountResolver::with_token_url(
        store,
        client.clone(),
        format!("{revoked_base}/token"),
    ));

    let state = Arc::new(AppState {
        config: ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![
                code_assist_provider(
                    "revoked-upstream",
                    &revoked_base,
                    Format::Gemini,
                    Some("gemini:me@example.com"),
                ),
                code_assist_provider("healthy-upstream", &healthy_base, Format::Gemini, None),
            ],
            model_routes: vec![ModelRouteConfig {
                pattern: "gemini-oauth".into(),
                provider: None,
                providers: vec!["revoked-upstream".into(), "healthy-upstream".into()],
                upstream_model: None,
                strategy: RouteStrategy::Priority,
            }],
            routes: vec![],
            model_metadata: Default::default(),
            logging: Default::default(),
        },
        client,
        accounts,
        usage_logger: None,
        request_logger: None,
        copilot: Arc::new(
            any_converter_server::credentials::copilot::CopilotSessions::new(Client::new()),
        ),
        hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
            Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                std::env::temp_dir().join("any-converter-test-plugin-auth.json"),
            )),
        )),
    });
    let app = create_router_with_state(state);

    revoked_upstream.push_generate_401(r#"{"error":{"message":"token_revoked"}}"#);

    // First request: the revoked provider 401s, the account is marked, and
    // the request fails over to the healthy provider.
    let (status, body) = chat_via(&app, chat_body(false)).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["choices"][0]["message"]["content"], "hi");
    assert_eq!(revoked_upstream.generate_calls(), 1);
    assert_eq!(healthy_upstream.generate_calls(), 1);

    // The revoked marker was persisted next to the secrets file.
    let revoked = std::fs::read_to_string(dir.path().join("revoked.json")).unwrap();
    assert!(revoked.contains("gemini:me@example.com"), "{revoked}");
    assert!(revoked.contains("token_revoked"), "{revoked}");

    // A second request never touches the revoked upstream again: the
    // revoked account is skipped (P9 multi-account semantics) and the
    // healthy provider serves instead of the old revoked guidance.
    let (status, body) = chat_via(&app, chat_body(false)).await;
    assert_eq!(
        revoked_upstream.generate_calls(),
        1,
        "revoked upstream must not be called again"
    );
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(healthy_upstream.generate_calls(), 2);
}

#[tokio::test]
async fn unknown_401_does_not_mark() {
    let env = TestEnv::build(
        Format::Gemini,
        Some("gemini:me@example.com"),
        Some(unix_now() + 3600),
    )
    .await;
    env.upstream
        .push_generate_401(r#"{"error":"unauthorized"}"#);

    let (_status, _body) = env.chat(chat_body(false)).await;

    // No marker file may be written and no retry may be attempted.
    assert!(
        !env.revoked_path().exists(),
        "revoked.json must not be created for an Unknown 401"
    );
    assert_eq!(env.upstream.generate_calls(), 1, "no retry on Unknown 401");
}

#[tokio::test]
async fn refresh_takes_the_file_lock() {
    let env = TestEnv::build(
        Format::Gemini,
        Some("gemini:me@example.com"),
        Some(unix_now() - 100),
    )
    .await;
    env.upstream.set_token_delay(300);

    let lock_path = {
        let mut path = env.secrets_path.clone().into_os_string();
        path.push(".refresh.lock");
        std::path::PathBuf::from(path)
    };

    let accounts = Arc::clone(&env.accounts);
    let refresh =
        tokio::spawn(async move { accounts.force_refresh("gemini:me@example.com").await });

    // The refresh takes the file lock before calling the token endpoint;
    // wait for the lock file to appear.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !lock_path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "refresh never took the file lock"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // While the refresh holds the lock, a second acquirer times out.
    let err = FileLock::new(lock_path.clone())
        .acquire_timeout(std::time::Duration::from_millis(150))
        .unwrap_err();
    assert!(
        matches!(err, LockError::Timeout(_)),
        "concurrent acquirer must be blocked: {err:?}"
    );

    let resolved = refresh.await.unwrap().unwrap();
    assert_eq!(resolved.access_token, "tok-2");

    // After the refresh the lock is released and the file removed.
    let guard = FileLock::new(lock_path.clone())
        .acquire_timeout(std::time::Duration::from_millis(500))
        .unwrap();
    drop(guard);
    assert!(!lock_path.exists(), "guard drop must remove the lock file");
}
