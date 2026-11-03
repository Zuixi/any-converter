//! End-to-end assertions for the Antigravity sign-in client (P13):
//!
//! Antigravity rides the same Google flow as Gemini, with its own client
//! id/secret, five scopes, `/oauth-callback` path, `localhost` loopback
//! host and daily Code Assist base. The Gemini defaults stay the
//! regression baseline (its URL must keep the historical parameters).
//!
//! No network is touched: only the authorize URLs are built and their
//! query parameters are checked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use std::collections::HashMap;

use any_converter_server::credentials::gemini::{
    ANTIGRAVITY_OAUTH_SCOPES, GEMINI_OAUTH_SCOPES, GeminiEndpoints, GeminiSignIn, GoogleApp,
};

/// Percent-decode one query component (`+` becomes space).
fn percent_decode(input: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi * 16 + lo);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_map(url: &str) -> HashMap<String, String> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or(url);
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

const GEMINI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";

#[tokio::test]
async fn antigravity_authorize_url_has_its_own_parameters() {
    let sign_in = GeminiSignIn::new(GeminiEndpoints::for_app(GoogleApp::Antigravity));
    let started = sign_in.start().await.unwrap();

    let (base, query) = started.url.split_once('?').unwrap();
    assert_eq!(base, "https://accounts.google.com/o/oauth2/v2/auth");
    let params = query_map(query);

    // Antigravity's own client id.
    assert_eq!(params.get("client_id").unwrap(), ANTIGRAVITY_CLIENT_ID);
    // All five scopes, space-joined: the three Gemini ones plus cclog and
    // experimentsandconfigs.
    assert_eq!(params.get("scope").unwrap(), ANTIGRAVITY_OAUTH_SCOPES);
    assert_eq!(params.get("scope").unwrap().split(' ').count(), 5);
    assert!(params.get("scope").unwrap().contains("cclog"));
    assert!(
        params
            .get("scope")
            .unwrap()
            .contains("experimentsandconfigs")
    );
    // The callback: `localhost` host, `/oauth-callback` path (Gemini uses
    // 127.0.0.1 + /oauth2callback).
    let redirect = params.get("redirect_uri").unwrap();
    assert!(
        redirect.starts_with("http://localhost:"),
        "redirect_uri: {redirect}"
    );
    assert!(
        redirect.ends_with("/oauth-callback"),
        "redirect_uri: {redirect}"
    );
    let port = redirect
        .strip_prefix("http://localhost:")
        .unwrap()
        .strip_suffix("/oauth-callback")
        .unwrap();
    assert_eq!(port.parse::<u16>().unwrap(), started.port);
    // PKCE shape is shared with Gemini.
    assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
    assert_eq!(params.get("code_challenge").unwrap().len(), 43);
    assert_eq!(params.get("access_type").unwrap(), "offline");
    assert_eq!(params.get("prompt").unwrap(), "consent");
}

#[tokio::test]
async fn gemini_authorize_url_keeps_the_historical_parameters() {
    let sign_in = GeminiSignIn::new(GeminiEndpoints::default());
    let started = sign_in.start().await.unwrap();

    let (base, query) = started.url.split_once('?').unwrap();
    assert_eq!(base, "https://accounts.google.com/o/oauth2/v2/auth");
    let params = query_map(query);

    assert_eq!(params.get("client_id").unwrap(), GEMINI_CLIENT_ID);
    // Still the three historical scopes, nothing added.
    assert_eq!(params.get("scope").unwrap(), GEMINI_OAUTH_SCOPES);
    assert_eq!(params.get("scope").unwrap().split(' ').count(), 3);
    assert!(!params.get("scope").unwrap().contains("cclog"));
    // Still 127.0.0.1 + /oauth2callback.
    let redirect = params.get("redirect_uri").unwrap();
    assert!(
        redirect.starts_with("http://127.0.0.1:"),
        "redirect_uri: {redirect}"
    );
    assert!(
        redirect.ends_with("/oauth2callback"),
        "redirect_uri: {redirect}"
    );
    assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
    assert_eq!(params.get("code_challenge").unwrap().len(), 43);
    assert_eq!(params.get("access_type").unwrap(), "offline");
    assert_eq!(params.get("prompt").unwrap(), "consent");
}

#[test]
fn antigravity_endpoints_carry_the_difference_table() {
    let antigravity = GeminiEndpoints::for_app(GoogleApp::Antigravity);
    assert_eq!(antigravity.app, GoogleApp::Antigravity);
    assert_eq!(antigravity.client_id, ANTIGRAVITY_CLIENT_ID);
    assert_eq!(
        antigravity.client_secret,
        "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"
    );
    assert_eq!(antigravity.scopes, ANTIGRAVITY_OAUTH_SCOPES);
    assert_eq!(antigravity.callback_path, "/oauth-callback");
    assert_eq!(antigravity.loopback_host, "localhost");
    assert_eq!(
        antigravity.base,
        "https://daily-cloudcode-pa.googleapis.com"
    );
    assert_eq!(antigravity.load_base, "https://cloudcode-pa.googleapis.com");

    let gemini = GeminiEndpoints::default();
    assert_eq!(gemini.app, GoogleApp::Gemini);
    assert_eq!(gemini.client_id, GEMINI_CLIENT_ID);
    assert_eq!(gemini.client_secret, "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");
    assert_eq!(gemini.callback_path, "/oauth2callback");
    assert_eq!(gemini.loopback_host, "127.0.0.1");
    assert_eq!(gemini.base, "https://cloudcode-pa.googleapis.com");
    assert_eq!(gemini.load_base, "https://cloudcode-pa.googleapis.com");
}

/// Google sets an Antigravity account up as Antigravity, not as Gemini CLI:
/// `loadCodeAssist` takes `{"metadata":{"ideType":"ANTIGRAVITY"}}`, and
/// onboarding names the tier in snake_case (magpie `antigravityProject`).
/// Asking as Gemini CLI gives the account a Gemini project and tier.
#[tokio::test]
async fn antigravity_onboards_with_its_own_identity() {
    use any_converter_server::credentials::{
        AccountResolver, Credential, FileSecretStore, SecretStore,
    };

    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hits = Arc::clone(&seen);
    let app = axum::Router::new()
        .route(
            "/v1internal:loadCodeAssist",
            axum::routing::post(move |body: axum::body::Bytes| {
                let hits = Arc::clone(&hits);
                async move {
                    hits.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&body).into_owned());
                    axum::Json(serde_json::json!({
                        "currentTier": { "id": "free-tier", "name": "Free" },
                        "cloudaicompanionProject": "proj-1",
                    }))
                }
            }),
        )
        .route(
            "/v1/internal/fetchAvailableModels",
            axum::routing::post(|| async { axum::Json(serde_json::json!({ "models": {} })) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let store = FileSecretStore::new(dir.path().join("secrets.json"));
    let id = "antigravity:me@example.com";
    store
        .put(
            id,
            &Credential::OAuth {
                provider: "antigravity".into(),
                account: "me@example.com".into(),
                access_token: "tok".into(),
                refresh_token: Some("r".into()),
                expires_at_unix: Some(now_unix() + 3600),
                siwc: None,
            },
        )
        .unwrap();
    let resolver = AccountResolver::new(store, reqwest::Client::new())
        .with_siwc_meta_path(dir.path().join("siwc.json"));
    let found = resolver.project(id, &base, None).await.unwrap();
    assert_eq!(found.id, "proj-1");

    let bodies = seen.lock().unwrap().clone();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("ANTIGRAVITY") && !b.contains("pluginType")),
        "Antigravity asks as Antigravity, not as Gemini CLI: {bodies:?}"
    );
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
