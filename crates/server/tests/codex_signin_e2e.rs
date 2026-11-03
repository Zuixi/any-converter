//! End-to-end tests for the Codex (ChatGPT) OAuth sign-in (P14).
//!
//! Every test injects its own endpoints and loopback port, so nothing
//! here touches the real port 1455 or the real auth.openai.com: the token
//! exchange goes to a local mock HTTP server started per test, and the
//! write-back home is a throwaway temp directory.
//!
//! 1. the authorize URL carries every parameter Codex CLI sends;
//! 2. the token exchange carries the `originator: codex_cli_rs` and
//!    `codex_cli_rs/<ver>` User-Agent headers;
//! 3. the account identity comes from the id token's claims;
//! 4. a team plan shows up in the display name;
//! 5. `auth_json()` has Codex's exact shape;
//! 6. a pasted callback finishes the flow (and a wrong state is refused);
//! 7. an id token without an email is refused;
//! 8. a successful sign-in writes the rotated tokens back to the codex
//!    home's auth.json, preserving everything else.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use any_converter_server::credentials::SignInError;
use any_converter_server::credentials::codex_signin::{
    CODEX_OAUTH_SCOPES, CodexAccount, CodexEndpoints, CodexSignIn, CodexStarted,
};
use axum::http::StatusCode;
use axum::routing::any;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// base64url without padding (the same alphabet the PKCE helpers use).
fn b64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32;
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6 & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        }
    }
    out
}

/// A forged (unverifiable) JWT: `{"alg":"none"}.{payload}.sig`.
fn fake_jwt(payload: serde_json::Value) -> String {
    let header = b64url(br#"{"alg":"none"}"#);
    let body = b64url(&serde_json::to_vec(&payload).unwrap());
    format!("{header}.{body}.sig")
}

/// The id token claims every test starts from (magpie `jwtClaims`).
fn claims(email: &str, plan: &str, account_id: &str) -> serde_json::Value {
    serde_json::json!({
        "email": email,
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": plan,
            "chatgpt_account_id": account_id,
        },
    })
}

/// The access token: a JWT whose own `exp` claim is the expiry.
fn access_token_jwt() -> String {
    fake_jwt(serde_json::json!({ "exp": 2_000_000_000 }))
}

/// A valid token exchange response (all three tokens present).
fn token_response(id_token: &str) -> serde_json::Value {
    serde_json::json!({
        "id_token": id_token,
        "access_token": access_token_jwt(),
        "refresh_token": "refresh-secret",
    })
}

/// Injected endpoints: the authorize side is a fake host, the token
/// exchange goes to `token_url` (a local mock), and the callback address
/// keeps Codex's fixed shape.
fn test_endpoints(token_url: String) -> CodexEndpoints {
    CodexEndpoints {
        authorize_url: "https://auth.example.test/oauth/authorize".to_string(),
        token_url,
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
        callback_addr: "http://localhost:1455/auth/callback".to_string(),
    }
}

/// The exchange request a mock captured (headers + form body).
#[derive(Clone, Default)]
struct Captured {
    originator: String,
    user_agent: String,
    content_type: String,
    body: String,
}

type Shared = Arc<Mutex<Option<Captured>>>;

/// One mock token endpoint: records the exchange and answers the scripted
/// JSON.
async fn spawn_mock(response: serde_json::Value) -> (String, Shared) {
    let captured: Shared = Arc::new(Mutex::new(None));
    let state = (captured.clone(), Arc::new(Mutex::new(response)));
    let app = axum::Router::new()
        .route(
            "/{*path}",
            any(
                |axum::extract::State((captured, response)): axum::extract::State<(
                    Shared,
                    Arc<Mutex<serde_json::Value>>,
                )>,
                 headers: axum::http::HeaderMap,
                 body: String| async move {
                    let get = |key: &str| {
                        headers
                            .get(key)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("")
                            .to_string()
                    };
                    *captured.lock().unwrap() = Some(Captured {
                        originator: get("originator"),
                        user_agent: get("user-agent"),
                        content_type: get("content-type"),
                        body,
                    });
                    let response = response.lock().unwrap().clone();
                    (StatusCode::OK, axum::Json(response))
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/oauth/token"), captured)
}

/// A sign-in wired to `token_url`, writing back to a throwaway home
/// (never the developer's real `~/.codex`) and taking an ephemeral
/// loopback port (never the real 1455).
fn test_sign_in(token_url: String) -> (CodexSignIn, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let sign_in = CodexSignIn::with_endpoints(test_endpoints(token_url))
        .with_home(dir.path().join(".codex"))
        .with_loopback_port(0);
    (sign_in, dir)
}

/// Finish a started flow with a pasted callback carrying the right state.
async fn finish(sign_in: &CodexSignIn, started: &CodexStarted) -> CodexAccount {
    let url = format!(
        "http://localhost:1455/auth/callback?state={}&code=pasted-code",
        started.state
    );
    sign_in.finish_pasted(&url, &started.state).await.unwrap()
}

fn query_map(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (pair.to_string(), String::new()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_authorize_url_has_every_parameter() {
    let (sign_in, _dir) = test_sign_in("http://127.0.0.1:1/oauth/token".to_string());
    let started = sign_in.start().await.unwrap();

    // Port 0 binds, so the callback server runs (no busy fallback).
    assert!(!started.paste_callback);
    assert_eq!(started.callback_blocked, "");

    let (base, query) = started.url.split_once('?').unwrap();
    assert_eq!(base, "https://auth.example.test/oauth/authorize");
    // The URL's parameters are percent-encoded; decode the redirect URI
    // and scope for value checks below.
    let params = query_map(query);
    assert_eq!(params.len(), 10, "params: {params:?}");
    assert_eq!(params.get("response_type").unwrap(), "code");
    assert_eq!(
        params.get("client_id").unwrap(),
        "app_EMoamEEZ73f0CkXaXp7hrann"
    );
    assert_eq!(
        params.get("redirect_uri").unwrap(),
        "http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"
    );
    assert_eq!(
        params.get("scope").unwrap(),
        "openid%20profile%20email%20offline_access%20api.connectors.read%20api.connectors.invoke"
    );
    assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
    assert_eq!(params.get("id_token_add_organizations").unwrap(), "true");
    assert_eq!(params.get("codex_cli_simplified_flow").unwrap(), "true");
    assert_eq!(params.get("state").unwrap(), &started.state);
    assert_eq!(params.get("originator").unwrap(), "codex_cli_rs");

    let challenge = params.get("code_challenge").unwrap();
    assert_eq!(challenge.len(), 43, "sha256 digest, unpadded base64url");
    assert!(!challenge.contains('+') && !challenge.contains('/') && !challenge.contains('='));
    let state = params.get("state").unwrap();
    assert_eq!(state.len(), 48, "24 bytes hex-encoded");

    // The scope constant itself must spell the six scopes.
    assert_eq!(CODEX_OAUTH_SCOPES.split(' ').count(), 6);
}

#[tokio::test]
async fn codex_exchange_uses_codex_headers() {
    let id_token = fake_jwt(claims("me@example.com", "pro", "acc-1"));
    let (token_url, captured) = spawn_mock(token_response(&id_token)).await;
    let (sign_in, _dir) = test_sign_in(token_url);
    let started = sign_in.start().await.unwrap();

    let account = finish(&sign_in, &started).await;
    assert_eq!(account.user, "me@example.com");

    let captured = captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured.originator, "codex_cli_rs");
    assert!(
        captured.user_agent.starts_with("codex_cli_rs/"),
        "user-agent: {}",
        captured.user_agent
    );
    assert_eq!(captured.content_type, "application/x-www-form-urlencoded");
    assert!(
        captured.body.contains("code_verifier="),
        "body: {}",
        captured.body
    );
    assert!(captured.body.contains("grant_type=authorization_code"));
    assert!(
        captured
            .body
            .contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback")
    );
}

#[tokio::test]
async fn codex_account_from_id_token_claims() {
    let id_token = fake_jwt(claims("me@example.com", "pro", "acc-123"));
    let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
    let (sign_in, _dir) = test_sign_in(token_url);
    let started = sign_in.start().await.unwrap();

    let account = finish(&sign_in, &started).await;
    assert_eq!(account.user, "me@example.com");
    assert_eq!(account.email, "me@example.com");
    assert_eq!(account.plan, "pro");
    assert_eq!(account.account_id, "acc-123");
    assert_eq!(account.account_id_key(), "codex:me@example.com");
    assert_eq!(account.refresh_token, "refresh-secret");
    assert_eq!(account.access_token, access_token_jwt());
    assert_eq!(account.expires_at_unix, Some(2_000_000_000));
}

#[tokio::test]
async fn codex_plan_team_is_shown_in_the_name() {
    let id_token = fake_jwt(claims("me@example.com", "team", "acc-1"));
    let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
    let (sign_in, _dir) = test_sign_in(token_url);
    let started = sign_in.start().await.unwrap();

    let account = finish(&sign_in, &started).await;
    assert_eq!(account.user, "me@example.com · Team");
    assert_eq!(account.plan, "team");
}

#[tokio::test]
async fn codex_auth_json_shape() {
    let id_token = fake_jwt(claims("me@example.com", "pro", "acc-1"));
    let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
    let (sign_in, _dir) = test_sign_in(token_url);
    let started = sign_in.start().await.unwrap();
    let account = finish(&sign_in, &started).await;

    let bytes = account.auth_json();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let obj = value.as_object().unwrap();
    assert_eq!(obj.len(), 4, "top-level keys: {:?}", obj.keys());
    assert!(obj.get("OPENAI_API_KEY").unwrap().is_null());
    assert_eq!(obj.get("auth_mode").unwrap(), "chatgpt");
    let last_refresh = obj.get("last_refresh").unwrap().as_str().unwrap();
    chrono::DateTime::parse_from_rfc3339(last_refresh)
        .unwrap_or_else(|err| panic!("last_refresh not RFC3339: {last_refresh}: {err}"));
    let tokens = obj.get("tokens").unwrap().as_object().unwrap();
    assert_eq!(tokens.len(), 4, "token keys: {:?}", tokens.keys());
    assert_eq!(tokens.get("id_token").unwrap(), &id_token);
    assert_eq!(tokens.get("access_token").unwrap(), &access_token_jwt());
    assert_eq!(tokens.get("refresh_token").unwrap(), "refresh-secret");
    assert_eq!(tokens.get("account_id").unwrap(), "acc-1");
    // Keys are sorted: re-serializing the parsed value is byte-identical.
    assert_eq!(serde_json::to_vec(&value).unwrap(), bytes);
}

#[tokio::test]
async fn codex_pasted_callback_finishes() {
    let id_token = fake_jwt(claims("me@example.com", "pro", "acc-1"));
    let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
    let (sign_in, _dir) = test_sign_in(token_url);
    let started = sign_in.start().await.unwrap();

    // A wrong state is refused before anything is consumed.
    let wrong = "http://localhost:1455/auth/callback?state=wrong-state&code=auth-code";
    let err = sign_in
        .finish_pasted(wrong, &started.state)
        .await
        .unwrap_err();
    assert!(matches!(err, SignInError::StateMismatch));

    // The correct state still completes the same flow.
    let account = finish(&sign_in, &started).await;
    assert_eq!(account.user, "me@example.com");
    assert_eq!(account.account_id_key(), "codex:me@example.com");
}

#[tokio::test]
async fn codex_missing_email_is_refused() {
    let id_token = fake_jwt(serde_json::json!({
        "https://api.openai.com/auth": { "chatgpt_plan_type": "pro" }
    }));
    let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
    let (sign_in, _dir) = test_sign_in(token_url);
    let started = sign_in.start().await.unwrap();

    let url = format!(
        "http://localhost:1455/auth/callback?state={}&code=pasted-code",
        started.state
    );
    let err = sign_in
        .finish_pasted(&url, &started.state)
        .await
        .unwrap_err();
    assert!(
        matches!(err, SignInError::Vendor(ref m) if m.contains("ChatGPT didn't say which account signed in")),
        "err: {err}"
    );
}

#[tokio::test]
async fn codex_signin_writes_back_to_auth_json() {
    // A temporary CODEX_HOME with an existing auth.json: the sign-in
    // updates its tokens and keeps every other field.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".codex");
    std::fs::create_dir_all(&home).unwrap();
    let auth_path = home.join("auth.json");
    std::fs::write(
        &auth_path,
        serde_json::json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "old-id",
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "account_id": "old-acc",
            },
            "last_refresh": "old",
            "custom_field": "keep-me",
        })
        .to_string(),
    )
    .unwrap();

    let id_token = fake_jwt(claims("me@example.com", "pro", "acc-9"));
    let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
    let sign_in = CodexSignIn::with_endpoints(test_endpoints(token_url))
        .with_home(home.clone())
        .with_loopback_port(0);
    let started = sign_in.start().await.unwrap();
    finish(&sign_in, &started).await;

    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_path).unwrap()).unwrap();
    // The rotated tokens are updated...
    assert_eq!(after["tokens"]["id_token"], id_token);
    assert_eq!(after["tokens"]["access_token"], access_token_jwt());
    assert_eq!(after["tokens"]["refresh_token"], "refresh-secret");
    assert_ne!(after["last_refresh"], "old");
    // ...and everything else is preserved untouched (write_tokens rotates
    // only what it owns: identity fields stay).
    assert_eq!(after["tokens"]["account_id"], "old-acc");
    assert_eq!(after["auth_mode"], "chatgpt");
    assert!(after["OPENAI_API_KEY"].is_null());
    assert_eq!(after["custom_field"], "keep-me");
    // The last-good backup exists (magpie's lastgood generation).
    assert!(home.join("auth.json.bak").is_file());
}
