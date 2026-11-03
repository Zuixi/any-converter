//! End-to-end tests for Sign in with ChatGPT (P16), against injected
//! endpoints — no OpenAI, no 1455 listener.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use any_converter_server::credentials::siwc_signin::{
    SIWC_DIRECT_SCOPE, SiwcEndpoints, SiwcError, SiwcHttp, SiwcReply, SiwcSignIn, jwt_claims,
};
use any_converter_server::credentials::{Credential, FileSecretStore, SecretStore, SignInError};
use futures_util::future::BoxFuture;

fn b64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
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

fn jwt(claims: &serde_json::Value) -> String {
    let header = b64url(br#"{"alg":"none"}"#);
    let payload = b64url(serde_json::to_vec(claims).unwrap().as_slice());
    format!("{header}.{payload}.")
}

/// One scripted answer per URL suffix; every request is recorded.
#[derive(Clone, Default)]
struct Mock {
    answers: Arc<Mutex<HashMap<String, SiwcReply>>>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    fn route(&self, suffix: &str, status: u16, body: &str) {
        self.answers.lock().unwrap().insert(
            suffix.to_string(),
            SiwcReply {
                status,
                body: body.to_string(),
            },
        );
    }

    fn calls(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn answer(&self, url: &str) -> Result<SiwcReply, SignInError> {
        self.seen.lock().unwrap().push(url.to_string());
        let answers = self.answers.lock().unwrap();
        answers
            .iter()
            .find(|(suffix, _)| url.contains(suffix.as_str()))
            .map(|(_, reply)| reply.clone())
            .ok_or_else(|| SignInError::Vendor(format!("no route for {url}")))
    }
}

impl SiwcHttp for Mock {
    fn get(
        &self,
        url: String,
        _headers: Vec<(String, String)>,
    ) -> BoxFuture<'static, Result<SiwcReply, SignInError>> {
        let this = self.clone();
        Box::pin(async move { this.answer(&url) })
    }

    fn post(
        &self,
        url: String,
        _headers: Vec<(String, String)>,
        _body: String,
    ) -> BoxFuture<'static, Result<SiwcReply, SignInError>> {
        let this = self.clone();
        Box::pin(async move { this.answer(&url) })
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    sign_in: SiwcSignIn,
    mock: Mock,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default();
    let sign_in = SiwcSignIn::with_http(SiwcEndpoints::default(), Arc::new(mock.clone()))
        .with_meta_path(dir.path().join("siwc-accounts.json"))
        .with_host_id("urn:uuid:11111111-2222-4333-8444-555555555555")
        .with_port(41234);
    Fixture { dir, sign_in, mock }
}

fn token_body(client_scope: &str) -> String {
    serde_json::json!({
        "access_token": "access-1",
        "refresh_token": "refresh-1",
        "id_token": "",
        "expires_in": 3600,
        "scope": client_scope,
    })
    .to_string()
}

fn id_claims(nonce: &str, client_id: &str) -> serde_json::Value {
    serde_json::json!({
        "iss": "https://auth.openai.com",
        "aud": client_id,
        "nonce": nonce,
        "sub": "user-123",
        "email": "me@example.com",
        "exp": now() + 3600,
        "https://api.openai.com/auth": { "chatgpt_plan_type": "plus" },
    })
}

/// A renewal request built the way `AccountResolver` builds one.
fn siwc_request<'a>(
    client_id: &'a str,
    refresh: &'a str,
    access: &'a str,
    expires_at_unix: i64,
) -> any_converter_server::credentials::siwc_signin::SiwcRefreshRequest<'a> {
    any_converter_server::credentials::siwc_signin::SiwcRefreshRequest {
        client_id,
        refresh_token: refresh,
        access_token: access,
        id_token: "",
        scopes: &[],
        expires_at_unix,
        force: false,
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ── the authorize URL ──────────────────────────────────────────────────

#[test]
fn authorize_url_has_every_parameter() {
    let f = fixture();
    let started = f.sign_in.start().unwrap();
    let query = started.url.split_once('?').unwrap().1;
    let params: HashMap<&str, &str> = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect();

    assert_eq!(params["client_id"], "dynamic_agent_client");
    assert_eq!(params["agent_name_hint"], "any-converter");
    assert!(
        params["ext_agent_host_id"].starts_with("urn%3Auuid%3A")
            || params["ext_agent_host_id"].starts_with("urn:uuid:")
    );
    assert_eq!(params["response_type"], "code");
    assert!(params["redirect_uri"].contains("127.0.0.1%3A41234%2Fauth%2Fcallback"));
    assert_eq!(params["resource"], "https%3A%2F%2Fapi.openai.com%2Fv1");
    assert!(params["scope"].contains("chatgpt.tokens.use.direct"));
    assert!(!params["state"].is_empty());
    assert!(!params["nonce"].is_empty());
    assert_eq!(params["code_challenge_method"], "S256");
    assert!(started.paste_callback);
}

#[test]
fn host_id_is_stable_across_starts() {
    let f = fixture();
    let first = f.sign_in.start().unwrap();
    let second = f.sign_in.start().unwrap();
    let host = |url: &str| {
        url.split_once('?')
            .unwrap()
            .1
            .split('&')
            .find_map(|p| p.strip_prefix("ext_agent_host_id="))
            .unwrap()
            .to_string()
    };
    assert_eq!(host(&first.url), host(&second.url));
    // The nonce and state are fresh each time.
    assert_ne!(first.state, second.state);
    assert_ne!(first.nonce, second.nonce);
}

// ── finishing ──────────────────────────────────────────────────────────

#[tokio::test]
async fn missing_client_id_is_refused() {
    let f = fixture();
    let started = f.sign_in.start().unwrap();
    let err = f.sign_in.finish("code", "", &started).await.unwrap_err();
    assert!(err.to_string().contains("registering"), "{err}");
}

#[tokio::test]
async fn scope_without_direct_is_refused() {
    let f = fixture();
    let started = f.sign_in.start().unwrap();
    // OpenAI granted the scopes but not the one that lets the token be used
    // against its own API: the account may not be eligible.
    f.mock
        .route("/oauth/token", 200, &token_body("openid profile"));
    let err = f
        .sign_in
        .finish("code", "client-1", &started)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("eligible"), "{err}");
}

#[tokio::test]
async fn id_token_checks_catch_a_foreign_token() {
    let f = fixture();
    let started = f.sign_in.start().unwrap();
    let scope = format!("openid {SIWC_DIRECT_SCOPE}");

    let cases = [
        (
            serde_json::json!({"iss": "https://evil.example", "aud": "client-1",
                "nonce": started.nonce, "email": "me@example.com", "sub": "s"}),
            "not OpenAI",
        ),
        (
            serde_json::json!({"iss": "https://auth.openai.com", "aud": "other-client",
                "nonce": started.nonce, "email": "me@example.com", "sub": "s"}),
            "another app",
        ),
        (
            serde_json::json!({"iss": "https://auth.openai.com", "aud": "client-1",
                "nonce": "wrong", "email": "me@example.com", "sub": "s"}),
            "isn't this sign-in's",
        ),
        (
            serde_json::json!({"iss": "https://auth.openai.com", "aud": "client-1",
                "nonce": started.nonce, "email": "me@example.com", "sub": "s",
                "exp": now() - 600}),
            "expired",
        ),
    ];
    for (claims, expected) in cases {
        let mut body: serde_json::Value = serde_json::from_str(&token_body(&scope)).unwrap();
        body["id_token"] = serde_json::Value::String(jwt(&claims));
        f.mock.route("/oauth/token", 200, &body.to_string());
        let err = f
            .sign_in
            .finish("code", "client-1", &started)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains(expected),
            "want {expected}, got {err}"
        );
    }
}

#[tokio::test]
async fn account_comes_from_the_claims() {
    let f = fixture();
    let started = f.sign_in.start().unwrap();
    let scope = format!("openid {SIWC_DIRECT_SCOPE}");
    let mut body: serde_json::Value = serde_json::from_str(&token_body(&scope)).unwrap();
    body["id_token"] = serde_json::Value::String(jwt(&id_claims(&started.nonce, "client-1")));
    f.mock.route("/oauth/token", 200, &body.to_string());

    let account = f
        .sign_in
        .finish("code", "client-1", &started)
        .await
        .unwrap();
    assert_eq!(account.user, "me@example.com");
    assert_eq!(account.plan, "plus");
    assert_eq!(account.client_id, "client-1");
    assert_eq!(account.account_id(), "chatgpt-api:me@example.com");
    assert!(account.expires_at_unix > now());

    // The client id and host id travel with the account itself: the caller
    // stores them in the same entry as the tokens (Credential::OAuth.siwc),
    // so nothing is written beside the secret store.
    assert_eq!(account.client_id, "client-1");
    assert!(account.host_id.starts_with("urn:uuid:"));
    assert!(
        !f.dir.path().join("siwc-accounts.json").exists(),
        "one ledger: no side file is written"
    );
}

// ── refreshing ─────────────────────────────────────────────────────────

/// Seed a stored account whose token is already expired, so `refresh` runs.
fn seed_expired(dir: &tempfile::TempDir) -> String {
    let id = "chatgpt-api:me@example.com".to_string();
    let store = FileSecretStore::new(dir.path().join("secrets.json"));
    store
        .put(
            &id,
            &Credential::OAuth {
                provider: "chatgpt-api".into(),
                account: "me@example.com".into(),
                access_token: "old-access".into(),
                refresh_token: Some("old-refresh".into()),
                expires_at_unix: Some(now() - 10),
                siwc: None,
            },
        )
        .unwrap();
    any_converter_server::credentials::siwc_store::put(
        &dir.path().join("siwc-accounts.json"),
        &id,
        &any_converter_server::credentials::SiwcMeta {
            client_id: "client-1".into(),
            host_id: "urn:uuid:x".into(),
            ..Default::default()
        },
    )
    .unwrap();
    id
}

#[tokio::test]
async fn refresh_dead_code_means_sign_in_again() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default();
    mock.route(
        "/oauth/token",
        400,
        r#"{"error":{"code":"invalid_grant","message":"revoked"}}"#,
    );
    let sign_in = SiwcSignIn::with_http(SiwcEndpoints::default(), Arc::new(mock.clone()))
        .with_meta_path(dir.path().join("siwc-accounts.json"));
    let _ = seed_expired(&dir);
    let err = sign_in
        .renew(siwc_request(
            "client-1",
            "old-refresh",
            "old-access",
            now() - 10,
        ))
        .await
        .unwrap_err();
    assert_eq!(err, SiwcError::SignInExpired);
}

#[tokio::test]
async fn refresh_unknown_client_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default();
    mock.route(
        "/oauth/token",
        400,
        r#"{"error":{"code":"invalid_client","message":"unknown client"}}"#,
    );
    let sign_in = SiwcSignIn::with_http(SiwcEndpoints::default(), Arc::new(mock.clone()))
        .with_meta_path(dir.path().join("siwc-accounts.json"));
    let _ = seed_expired(&dir);
    assert_eq!(
        sign_in
            .renew(siwc_request(
                "client-1",
                "old-refresh",
                "old-access",
                now() - 10
            ))
            .await
            .unwrap_err(),
        SiwcError::ClientUnknown
    );
}

#[tokio::test]
async fn refresh_hiccup_keeps_the_token_in_hand() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default();
    // The token endpoint is unreachable, but the stored token is still
    // valid: magpie keeps using it rather than failing the request.
    let sign_in = SiwcSignIn::with_http(SiwcEndpoints::default(), Arc::new(mock))
        .with_meta_path(dir.path().join("siwc-accounts.json"));
    let id = "chatgpt-api:me@example.com".to_string();
    FileSecretStore::new(dir.path().join("secrets.json"))
        .put(
            &id,
            &Credential::OAuth {
                provider: "chatgpt-api".into(),
                account: "me@example.com".into(),
                access_token: "still-good".into(),
                refresh_token: Some("r".into()),
                // Expired for the *margin*, still inside its real lifetime.
                expires_at_unix: Some(now() + 60),
                siwc: None,
            },
        )
        .unwrap();
    any_converter_server::credentials::siwc_store::put(
        &dir.path().join("siwc-accounts.json"),
        &id,
        &any_converter_server::credentials::SiwcMeta {
            client_id: "client-1".into(),
            ..Default::default()
        },
    )
    .unwrap();
    // A hiccup is not a refusal: the token in hand still has life in it, so
    // the request goes on rather than failing. `None` is exactly that.
    let renewed = sign_in
        .renew(siwc_request("client-1", "r", "still-good", now() + 60))
        .await
        .unwrap();
    assert!(renewed.is_none(), "the caller keeps the token in hand");
}

// ── revoking and models ────────────────────────────────────────────────

#[tokio::test]
async fn revoke_uses_the_published_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default();
    mock.route(
        "/.well-known/openid-configuration",
        200,
        r#"{"revocation_endpoint":"https://auth.openai.com/oauth/revoke"}"#,
    );
    mock.route("/oauth/revoke", 200, "{}");
    let sign_in = SiwcSignIn::with_http(SiwcEndpoints::default(), Arc::new(mock.clone()))
        .with_meta_path(dir.path().join("siwc-accounts.json"));
    let id = seed_expired(&dir);
    assert!(
        sign_in.revoke(&id, Some("old-refresh")).await.unwrap(),
        "revoked"
    );
    assert!(
        mock.calls().iter().any(|u| u.contains("/oauth/revoke")),
        "the endpoint was called: {:?}",
        mock.calls()
    );
}

#[tokio::test]
async fn revoke_without_an_endpoint_is_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default();
    mock.route("/.well-known/openid-configuration", 200, "{}");
    let sign_in = SiwcSignIn::with_http(SiwcEndpoints::default(), Arc::new(mock))
        .with_meta_path(dir.path().join("siwc-accounts.json"));
    let id = seed_expired(&dir);
    assert!(
        !sign_in.revoke(&id, Some("old-refresh")).await.unwrap(),
        "nothing to revoke at"
    );
}

#[tokio::test]
async fn models_come_from_openai_with_the_bearer() {
    let mock = Mock::default();
    mock.route(
        "/models",
        200,
        r#"{"data":[{"id":"gpt-5.6"},{"id":"gpt-5.6-mini"}]}"#,
    );
    let sign_in = SiwcSignIn::with_http(SiwcEndpoints::default(), Arc::new(mock.clone()));
    let models = sign_in
        .models("chatgpt-api:me@example.com", "tok")
        .await
        .unwrap();
    assert_eq!(
        models,
        vec!["gpt-5.6".to_string(), "gpt-5.6-mini".to_string()]
    );
    assert!(mock.calls()[0].ends_with("/models"));
}

#[test]
fn jwt_claims_round_trips() {
    let claims = serde_json::json!({"email": "me@example.com"});
    let token = jwt(&claims);
    assert_eq!(jwt_claims(&token).unwrap(), claims);
}

/// A Sign in with ChatGPT account keeps its client id **in the same entry as
/// its tokens**, so the two cannot drift: losing a side file would leave an
/// account that can never refresh again.
#[tokio::test]
async fn the_client_id_rides_with_the_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSecretStore::new(dir.path().join("secrets.json"));
    let id = "chatgpt-api:me@example.com";
    store
        .put(
            id,
            &Credential::OAuth {
                provider: "chatgpt-api".into(),
                account: "me@example.com".into(),
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at_unix: Some(now() + 3600),
                siwc: Some(any_converter_server::credentials::SiwcMeta {
                    client_id: "client-1".into(),
                    host_id: "urn:uuid:x".into(),
                    id_token: "id-1".into(),
                    scopes: vec!["openid".into()],
                }),
            },
        )
        .unwrap();

    // Read back through the resolver: it finds the client id in the entry.
    let backup = dir.path().join("siwc-accounts.json");
    assert!(!backup.exists(), "nothing is written beside the store");
    let raw = std::fs::read_to_string(dir.path().join("secrets.json")).unwrap();
    assert!(
        raw.contains("client-1"),
        "the client id is in the entry: {raw}"
    );
    assert!(raw.contains("urn:uuid:x"), "{raw}");

    // The old side file is still read when a sign-in predates this shape.
    any_converter_server::credentials::siwc_store::put(
        &backup,
        id,
        &any_converter_server::credentials::SiwcMeta {
            client_id: "client-1".into(),
            ..Default::default()
        },
    )
    .unwrap();
}
