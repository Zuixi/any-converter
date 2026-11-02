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
}

/// The non-streaming Code Assist endpoint: record the request, answer with an
/// enveloped `GenerateContentResponse`.
async fn generate_content(
    AxumState(state): AxumState<Arc<FakeUpstream>>,
    req: AxumRequest,
) -> Response {
    record_generate_request(&state, req).await;
    axum::Json(json!({
        "response": {
            "candidates": [{
                "content": {"parts": [{"text": "hi"}], "role": "model"}
            }]
        }
    }))
    .into_response()
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
        wire: None,
        model_map: Default::default(),
        endpoints: Default::default(),
        auth: Default::default(),
    }
}

fn server_config(provider: ProviderConfig) -> ServerConfig {
    ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
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
            accounts,
            usage_logger: None,
            request_logger: None,
        });
        let app = create_router_with_state(state);

        Self {
            _dir: dir,
            app,
            upstream,
            secrets_path,
        }
    }

    async fn chat(&self, body: Value) -> (StatusCode, String) {
        let response = self
            .app
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
