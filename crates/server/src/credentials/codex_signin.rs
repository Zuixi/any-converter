//! Codex CLI-style ChatGPT OAuth sign-in (PKCE + fixed loopback port 1455).
//!
//! Flow (mirrors magpie `codexLogin` / `signin.go:363-390, 834-880`):
//!
//! 1. [`CodexSignIn::start`] generates a PKCE pair, tries to take
//!    `127.0.0.1:1455` (OpenAI only redirects to that fixed address) and
//!    returns the authorize URL. When the port stays taken after asking a
//!    lingering `codex login` to leave (`GET /cancel`, 10 tries), the flow
//!    continues without a callback server: the browser lands on an address
//!    carrying the `code`, which the user pastes to finish (magpie #1782).
//! 2. [`CodexSignIn::wait`] serves `GET /auth/callback` on the held port
//!    until a callback with a matching `state` arrives, then exchanges the
//!    code for tokens.
//! 3. [`CodexSignIn::finish_pasted`] accepts the pasted callback URL and
//!    runs the same exchange path.
//!
//! The exchange posts to `https://auth.openai.com/oauth/token` with the
//! `originator: codex_cli_rs` and `codex_cli_rs/<ver>` User-Agent headers;
//! the account identity comes from the `id_token`'s claims, decoded
//! without checking the signature (the tokens are the user's own). On
//! success the tokens are written back to the Codex home's `auth.json`
//! ([`super::codex_store::write_tokens`]); a refused write-back (keyring
//! store, no auth.json) never fails the sign-in — it is warned and
//! skipped, and the account stays usable.
//!
//! All network interaction goes through reqwest against the configured
//! [`CodexEndpoints`], so tests inject local fakes; [`CodexSignIn::with_loopback_port`]
//! keeps tests off the real port 1455.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::any;

use super::codex_store::{TokenUpdate, write_tokens};
use super::gemini::{
    ERROR_PAGE, MISMATCH_PAGE, SUCCESS_PAGE, SignInError, base64url, base64url_decode,
    form_urlencoded, hex_encode, percent_encode, query_pairs, random_bytes, sha256,
};

/// The OAuth scopes Codex CLI's client asks for (space-joined).
pub const CODEX_OAUTH_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// The `originator` header value the authorize URL and the token exchange
/// carry (magpie `codex_cli_rs`).
pub const CODEX_ORIGINATOR: &str = "codex_cli_rs";

/// OAuth endpoints and client configuration for the Codex ChatGPT flow.
///
/// Injectable so tests can point at fake endpoints; [`Default`] yields
/// Codex CLI's own public OAuth client and OpenAI's real endpoints.
#[derive(Debug, Clone)]
pub struct CodexEndpoints {
    /// Authorization endpoint.
    pub authorize_url: String,
    /// Token exchange endpoint.
    pub token_url: String,
    /// Codex CLI's public OAuth client id (a public-client identifier,
    /// not a secret).
    pub client_id: String,
    /// The fixed redirect URI OpenAI answers to, port 1455 included
    /// (`http://localhost:1455/auth/callback`).
    pub callback_addr: String,
}

impl Default for CodexEndpoints {
    fn default() -> Self {
        Self {
            authorize_url: "https://auth.openai.com/oauth/authorize".to_string(),
            token_url: "https://auth.openai.com/oauth/token".to_string(),
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            callback_addr: "http://localhost:1455/auth/callback".to_string(),
        }
    }
}

/// What [`CodexSignIn::start`] produced: the authorize URL plus how the
/// flow can finish.
#[derive(Debug, Clone)]
pub struct CodexStarted {
    /// Full authorize URL to open in a browser.
    pub url: String,
    /// The loopback callback could not be served: the flow can only
    /// finish through [`CodexSignIn::finish_pasted`] with the address the
    /// browser finally lands on.
    pub paste_callback: bool,
    /// Why the callback server is not running: `"busy"` when the loopback
    /// port stayed taken, empty when the server owns the port.
    pub callback_blocked: String,
    /// Generated `state`; pass it back to `finish_pasted`.
    pub state: String,
}

/// A fully signed-in ChatGPT account (Codex CLI's own OAuth client).
#[derive(Debug, Clone)]
pub struct CodexAccount {
    /// Display name (magpie `codexUser`): the email, plus `" · Team"` when
    /// the plan is team/business/enterprise/edu.
    pub user: String,
    /// The email the id token claims; the stable account identity.
    pub email: String,
    /// The ChatGPT plan from the id token (`chatgpt_plan_type`).
    pub plan: String,
    /// The ChatGPT account id from the id token (`chatgpt_account_id`).
    pub account_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    /// The access token's own JWT `exp` claim in unix seconds (ChatGPT's
    /// token endpoint answers no `expires_in`).
    pub expires_at_unix: Option<i64>,
}

impl CodexAccount {
    /// The `auth.json` content Codex reads back (magpie `codexLogin`),
    /// with sorted top-level and token keys.
    pub fn auth_json(&self) -> Vec<u8> {
        let value = serde_json::json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "last_refresh": chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            "tokens": {
                "account_id": self.account_id,
                "access_token": self.access_token,
                "id_token": self.id_token,
                "refresh_token": self.refresh_token,
            },
        });
        serde_json::to_vec(&value).unwrap_or_default()
    }

    /// The account id [`super::AccountResolver`] stores this account under.
    pub fn account_id_key(&self) -> String {
        format!("codex:{}", self.email)
    }
}

/// Everything `wait`/`finish_pasted` need that was generated in `start`.
struct PkceSession {
    verifier: String,
    state: String,
}

/// Drives one Codex ChatGPT OAuth sign-in against configurable endpoints.
///
/// Usage: `start()` → user opens the URL → either `wait(timeout)`
/// (loopback callback) or `finish_pasted(url, state)` (paste fallback).
/// `wait` consumes the sign-in; `finish_pasted` borrows it (a second call
/// fails — the PKCE session is consumed on first use).
pub struct CodexSignIn {
    endpoints: CodexEndpoints,
    client: reqwest::Client,
    /// Loopback port to take (default 1455; `with_loopback_port(0)` takes
    /// an ephemeral one in tests).
    port: u16,
    /// The CODEX_HOME whose auth.json receives the tokens (`None`:
    /// `$CODEX_HOME`, else `~/.codex`).
    home: Option<PathBuf>,
    /// Created by `start`, consumed by the first exchange (`wait` or
    /// `finish_pasted`).
    session: Arc<tokio::sync::Mutex<Option<PkceSession>>>,
    /// The loopback listener `start` held; taken out by `wait`.
    listener: Arc<tokio::sync::Mutex<Option<tokio::net::TcpListener>>>,
}

impl Default for CodexSignIn {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexSignIn {
    /// Create a sign-in for the default endpoints taking port 1455.
    pub fn new() -> Self {
        Self::with_endpoints(CodexEndpoints::default())
    }

    /// Create a sign-in against specific endpoints (tests).
    pub fn with_endpoints(endpoints: CodexEndpoints) -> Self {
        Self::with_client(endpoints, None, 1455, None)
    }

    /// Take `port` instead of 1455 (tests pass `0` to never touch the
    /// real port; the redirect URI stays the endpoints' callback address).
    pub fn with_loopback_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Write the tokens back to `home`'s auth.json (test seam; production
    /// resolves `$CODEX_HOME`, else `~/.codex`).
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    /// Create a sign-in whose HTTP exchange leaves through `proxy` when
    /// set (P10, magpie `ProxyOfLogin`).
    pub fn with_proxy(endpoints: CodexEndpoints, proxy: Option<&str>) -> Result<Self, SignInError> {
        let client = crate::config::build_proxied_client(proxy)
            .map_err(|error| SignInError::Proxy(error.to_string()))?;
        Ok(Self::with_client(endpoints, Some(client), 1455, None))
    }

    fn with_client(
        endpoints: CodexEndpoints,
        client: Option<reqwest::Client>,
        port: u16,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            endpoints,
            client: client.unwrap_or_default(),
            port,
            home,
            session: Arc::new(tokio::sync::Mutex::new(None)),
            listener: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Generate the PKCE pair, try to take the loopback port and build the
    /// authorize URL. Call this before `wait`/`finish_pasted`; calling it
    /// again replaces any previous session.
    ///
    /// Port contention never fails the flow (magpie #1782): after asking a
    /// lingering `codex login` to leave (`GET /cancel`, 10 tries), a
    /// still-taken port makes the flow paste-only
    /// (`callback_blocked = "busy"`).
    pub async fn start(&self) -> Result<CodexStarted, SignInError> {
        // 32 random bytes → 64 hex chars, inside the RFC 7636 43..=128
        // range (shared with the Gemini sign-in's PKCE helpers).
        let verifier = hex_encode(&random_bytes(32));
        let state = hex_encode(&random_bytes(24));
        let challenge = base64url(&sha256(verifier.as_bytes()));

        let (listener, callback_blocked) = match self.take_port().await {
            Ok(listener) => (Some(listener), String::new()),
            Err(()) => (None, "busy".to_string()),
        };

        let url = build_authorize_url(&self.endpoints, &state, &challenge);

        *self.session.lock().await = Some(PkceSession {
            verifier,
            state: state.clone(),
        });
        *self.listener.lock().await = listener;

        Ok(CodexStarted {
            url,
            paste_callback: callback_blocked == "busy",
            callback_blocked,
            state,
        })
    }

    /// Take `127.0.0.1:<port>`, first asking whatever holds it to leave
    /// (magpie #1782: a `codex login` left behind answers `GET /cancel`).
    /// 10 tries, 200ms apart; `Err(())` when the port stays taken.
    async fn take_port(&self) -> Result<tokio::net::TcpListener, ()> {
        // The cancel probe is a fire-and-forget courtesy call: a short
        // timeout keeps a hostile (never-answering) port holder from
        // stalling the sign-in for long.
        let cancel_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .no_proxy()
            .build()
            .map_err(|_| ())?;
        for _ in 0..10 {
            match tokio::net::TcpListener::bind(("127.0.0.1", self.port)).await {
                Ok(listener) => return Ok(listener),
                Err(_) => {
                    let _ = cancel_client
                        .get(format!("http://127.0.0.1:{}/cancel", self.port))
                        .send()
                        .await;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
        Err(())
    }

    /// Serve the loopback callback until a matching `code` arrives, then
    /// exchange it. A callback with a mismatched `state` gets an error
    /// page and the wait continues; a callback carrying an `error` param
    /// fails the wait with [`SignInError::Vendor`]. No callback before
    /// `timeout` → [`SignInError::Timeout`].
    pub async fn wait(self, timeout: Duration) -> Result<CodexAccount, SignInError> {
        self.wait_inner(timeout).await
    }

    /// [`Self::wait`] without consuming the sign-in: the task registry's
    /// background wait shares the instance with the paste fallback, the
    /// same way the Gemini sign-in does.
    pub(crate) async fn wait_inner(&self, timeout: Duration) -> Result<CodexAccount, SignInError> {
        let (expected_state, listener) = {
            let expected_state = self
                .session
                .lock()
                .await
                .as_ref()
                .ok_or(SignInError::NotStarted)?
                .state
                .clone();
            let listener = self
                .listener
                .lock()
                .await
                .take()
                .ok_or(SignInError::NotStarted)?;
            (expected_state, listener)
        };

        let callback_path = callback_path_of(&self.endpoints.callback_addr);
        let (done_tx, mut done_rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<String, SignInError>>();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let app = axum::Router::new().route(
            callback_path.as_str(),
            any(move |Query(params): Query<HashMap<String, String>>| {
                let tx = done_tx.clone();
                let expected = expected_state.clone();
                async move {
                    match classify_callback(&params, &expected) {
                        Ok(code) => {
                            let _ = tx.send(Ok(code));
                            (StatusCode::OK, Html(SUCCESS_PAGE))
                        }
                        Err(SignInError::StateMismatch) => {
                            (StatusCode::BAD_REQUEST, Html(MISMATCH_PAGE))
                        }
                        Err(err) => {
                            let _ = tx.send(Err(err));
                            (StatusCode::BAD_REQUEST, Html(ERROR_PAGE))
                        }
                    }
                }
            }),
        );

        let _server = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        let outcome = tokio::time::timeout(timeout, done_rx.recv()).await;
        // Draining shutdown: also fires when `shutdown_tx` drops on return.
        let _ = shutdown_tx.send(());

        let code = match outcome {
            Err(_) => return Err(SignInError::Timeout),
            Ok(Some(Ok(code))) => code,
            Ok(Some(Err(err))) => return Err(err),
            Ok(None) => {
                return Err(SignInError::Vendor(
                    "callback server stopped unexpectedly".to_string(),
                ));
            }
        };

        let session = self
            .session
            .lock()
            .await
            .take()
            .ok_or(SignInError::NotStarted)?;
        self.complete(code, session.verifier).await
    }

    /// Paste fallback for when the browser cannot reach the loopback
    /// listener (or port 1455 was taken): parse the pasted callback URL,
    /// check `state`, then run the same exchange path as [`Self::wait`].
    pub async fn finish_pasted(
        &self,
        callback_url: &str,
        expected_state: &str,
    ) -> Result<CodexAccount, SignInError> {
        let pairs = query_pairs(callback_url);
        let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

        if let Some(err) = get("error") {
            let description = get("error_description").unwrap_or_default();
            return Err(SignInError::Vendor(format!(
                "authorization failed: {err} ({description})"
            )));
        }
        match get("state") {
            Some(state) if state == expected_state => {}
            // Missing or wrong state: reject before touching the session or
            // the token endpoint (nothing is consumed).
            _ => return Err(SignInError::StateMismatch),
        }
        let code = get("code").ok_or_else(|| {
            SignInError::BadUrl("callback url carries no authorization code".to_string())
        })?;

        let session = self
            .session
            .lock()
            .await
            .take()
            .ok_or(SignInError::NotStarted)?;
        self.complete(code, session.verifier).await
    }

    /// Shared tail of both flows: code → tokens → identity → write-back.
    async fn complete(&self, code: String, verifier: String) -> Result<CodexAccount, SignInError> {
        // The workspace enables no reqwest "form" feature, so the
        // urlencoded body is assembled by hand (shared helper).
        let body = form_urlencoded(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", self.endpoints.callback_addr.as_str()),
            ("client_id", self.endpoints.client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ]);
        let response = self
            .client
            .post(&self.endpoints.token_url)
            .header("originator", CODEX_ORIGINATOR)
            .header(reqwest::header::USER_AGENT, codex_user_agent())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(SignInError::Vendor(format!(
                "token endpoint returned {status}: {text}"
            )));
        }
        #[derive(serde::Deserialize)]
        struct Tokens {
            #[serde(default)]
            id_token: Option<String>,
            #[serde(default)]
            access_token: Option<String>,
            #[serde(default)]
            refresh_token: Option<String>,
        }
        let tokens: Tokens = serde_json::from_str(&text).map_err(|e| {
            SignInError::Vendor(format!("token endpoint returned malformed payload: {e}"))
        })?;
        // All three tokens are required: the sign-in is unusable without
        // them (magpie fails the same way).
        let (id_token, access_token, refresh_token) =
            match (tokens.id_token, tokens.access_token, tokens.refresh_token) {
                (Some(id_token), Some(access_token), Some(refresh_token)) => {
                    (id_token, access_token, refresh_token)
                }
                _ => {
                    return Err(SignInError::Vendor(
                        "ChatGPT token exchange came back without the tokens needed \
                         (id_token, access_token, refresh_token)"
                            .to_string(),
                    ));
                }
            };

        let account = account_from_claims(&id_token, access_token, refresh_token)?;

        // Write back to where Codex reads it. A refused write-back (a
        // keyring/auto/ephemeral store, or no auth.json to update) never
        // fails the sign-in: any-converter already rejects keychains, and
        // an absent auth.json means there is nothing to update — the
        // account stays usable either way.
        let home = self.home.clone().unwrap_or_else(default_codex_home);
        let update = TokenUpdate {
            access_token: account.access_token.clone(),
            refresh_token: account.refresh_token.clone(),
            id_token: Some(account.id_token.clone()),
        };
        if let Err(error) = write_tokens(&home, &update) {
            log::warn!(
                "codex sign-in write-back skipped for {}: {error}",
                home.display()
            );
        }
        Ok(account)
    }
}

/// Resolve the account identity from the `id_token`'s claims, decoded
/// without checking the signature (magpie `jwtClaims` + `codexUser`).
fn account_from_claims(
    id_token: &str,
    access_token: String,
    refresh_token: String,
) -> Result<CodexAccount, SignInError> {
    let claims = id_token_claims(id_token);
    let email = claims
        .as_ref()
        .and_then(|claims| claims.get("email"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if email.is_empty() {
        return Err(SignInError::Vendor(
            "ChatGPT didn't say which account signed in".to_string(),
        ));
    }
    let plan = claim_str(
        claims.as_ref(),
        &["https://api.openai.com/auth", "chatgpt_plan_type"],
    )
    .to_string();
    let account_id = claim_str(
        claims.as_ref(),
        &["https://api.openai.com/auth", "chatgpt_account_id"],
    )
    .to_string();
    Ok(CodexAccount {
        user: codex_user(&email, &plan),
        email,
        plan,
        account_id,
        access_token: access_token.clone(),
        refresh_token,
        id_token: id_token.to_string(),
        expires_at_unix: access_token_exp(&access_token),
    })
}

/// The payload of a JWT, decoded without checking its signature.
fn id_token_claims(id_token: &str) -> Option<serde_json::Value> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// The access token's own JWT `exp` claim in unix seconds (ChatGPT's
/// token endpoint answers no `expires_in`).
fn access_token_exp(access_token: &str) -> Option<i64> {
    id_token_claims(access_token)?
        .get("exp")
        .and_then(serde_json::Value::as_i64)
}

/// The string a claim path reads, `""` when any step is missing or not a
/// string.
fn claim_str<'a>(claims: Option<&'a serde_json::Value>, path: &[&str]) -> &'a str {
    let mut claims = match claims {
        Some(claims) => claims,
        None => return "",
    };
    for key in path {
        let Some(next) = claims.get(*key) else {
            return "";
        };
        claims = next;
    }
    claims.as_str().unwrap_or("")
}

/// The display name magpie shows for a ChatGPT account (`codexUser`):
/// the email; team/business/enterprise/edu plans add `" · <Plan>"`.
fn codex_user(email: &str, plan: &str) -> String {
    let plan_label = match plan {
        "team" | "business" | "enterprise" | "edu" => {
            let mut chars = plan.chars();
            let head = chars.next().map(|c| c.to_ascii_uppercase()).unwrap_or('?');
            let tail: String = chars.collect();
            format!("{head}{tail}")
        }
        _ => return email.to_string(),
    };
    format!("{email} · {plan_label}")
}

/// `codex_cli_rs/<ver> (<os>; <arch>) <term>` (magpie `codexUserAgent`).
fn codex_user_agent() -> String {
    let term = std::env::var("TERM").unwrap_or_default();
    format!(
        "codex_cli_rs/{} ({}; {}) {term}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// `$CODEX_HOME`, else `$HOME/.codex` (magpie `appdir.CodexHomes`' main
/// home; `$USERPROFILE` stands in for `$HOME` on Windows).
fn default_codex_home() -> PathBuf {
    if let Some(home) = std::env::var("CODEX_HOME")
        .ok()
        .filter(|home| !home.trim().is_empty())
    {
        return PathBuf::from(home);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    if home.is_empty() {
        return PathBuf::from(".codex");
    }
    PathBuf::from(home).join(".codex")
}

/// The path the callback server serves, taken from the callback address
/// (`/auth/callback` for the default endpoints).
fn callback_path_of(callback_addr: &str) -> String {
    reqwest::Url::parse(callback_addr)
        .ok()
        .map(|url| url.path().to_string())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| "/auth/callback".to_string())
}

/// Classify one inbound callback request: `Ok(code)` when the state
/// matches and a code is present, [`SignInError::StateMismatch`] to keep
/// waiting, [`SignInError::Vendor`] when the provider redirected with
/// `error=...`.
fn classify_callback(
    params: &HashMap<String, String>,
    expected_state: &str,
) -> Result<String, SignInError> {
    if let Some(err) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("");
        return Err(SignInError::Vendor(format!(
            "authorization failed: {err} ({description})"
        )));
    }
    match params.get("state") {
        Some(state) if state == expected_state => match params.get("code") {
            Some(code) if !code.is_empty() => Ok(code.clone()),
            _ => Err(SignInError::StateMismatch),
        },
        _ => Err(SignInError::StateMismatch),
    }
}

/// Assemble the authorize URL (all values percent-encoded; magpie
/// `signin.go:363-390`).
fn build_authorize_url(endpoints: &CodexEndpoints, state: &str, challenge: &str) -> String {
    let params: [(&str, String); 10] = [
        ("response_type", "code".to_string()),
        ("client_id", percent_encode(&endpoints.client_id)),
        ("redirect_uri", percent_encode(&endpoints.callback_addr)),
        ("scope", percent_encode(CODEX_OAUTH_SCOPES)),
        ("code_challenge", percent_encode(challenge)),
        ("code_challenge_method", "S256".to_string()),
        ("id_token_add_organizations", "true".to_string()),
        ("codex_cli_simplified_flow", "true".to_string()),
        ("state", percent_encode(state)),
        ("originator", CODEX_ORIGINATOR.to_string()),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{}", endpoints.authorize_url, query)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn test_endpoints(token_url: String) -> CodexEndpoints {
        CodexEndpoints {
            authorize_url: "https://auth.example.test/oauth/authorize".to_string(),
            token_url,
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            callback_addr: "http://localhost:1455/auth/callback".to_string(),
        }
    }

    fn fake_jwt(payload: serde_json::Value) -> String {
        let header = base64url(br#"{"alg":"none"}"#);
        let body = base64url(&serde_json::to_vec(&payload).unwrap());
        format!("{header}.{body}.sig")
    }

    fn token_response(id_token: &str) -> String {
        serde_json::json!({
            "id_token": id_token,
            "access_token": fake_jwt(serde_json::json!({ "exp": 2_000_000_000 })),
            "refresh_token": "refresh-secret",
        })
        .to_string()
    }

    fn claims(email: &str, plan: &str, account_id: &str) -> serde_json::Value {
        serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": plan,
                "chatgpt_account_id": account_id,
            },
        })
    }

    /// One mock token endpoint: captures the exchange headers/body and
    /// answers the scripted JSON.
    #[derive(Clone)]
    struct MockState {
        captured: Arc<std::sync::Mutex<HashMap<String, String>>>,
        response: Arc<std::sync::Mutex<String>>,
    }

    async fn spawn_mock(
        response: String,
    ) -> (String, Arc<std::sync::Mutex<HashMap<String, String>>>) {
        let captured = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let state = MockState {
            captured: captured.clone(),
            response: Arc::new(std::sync::Mutex::new(response)),
        };
        let app = axum::Router::new()
            .route(
                "/{*path}",
                any(
                    |axum::extract::State(state): axum::extract::State<MockState>,
                     headers: axum::http::HeaderMap,
                     body: String| async move {
                        let mut captured = state.captured.lock().unwrap();
                        for key in ["originator", "user-agent", "content-type"] {
                            captured.insert(
                                key.to_string(),
                                headers
                                    .get(key)
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or("")
                                    .to_string(),
                            );
                        }
                        captured.insert("body".to_string(), body);
                        let response: serde_json::Value =
                            serde_json::from_str(&state.response.lock().unwrap()).unwrap();
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
    fn test_sign_in(token_url: String) -> CodexSignIn {
        let dir = tempfile::tempdir().unwrap();
        let sign_in = CodexSignIn::with_endpoints(test_endpoints(token_url))
            .with_home(dir.path().join(".codex"))
            .with_loopback_port(0);
        // Keep the temp dir alive for the whole test.
        std::mem::forget(dir);
        sign_in
    }

    async fn finish(sign_in: &CodexSignIn, started: &CodexStarted) -> CodexAccount {
        let url = format!(
            "http://localhost:1455/auth/callback?state={}&code=pasted-code",
            started.state
        );
        sign_in.finish_pasted(&url, &started.state).await.unwrap()
    }

    #[tokio::test]
    async fn authorize_url_has_every_parameter() {
        let sign_in = test_sign_in("http://127.0.0.1:1/oauth/token".to_string());
        let started = sign_in.start().await.unwrap();

        assert!(!started.paste_callback);
        assert_eq!(started.callback_blocked, "");

        let (base, query) = started.url.split_once('?').unwrap();
        assert_eq!(base, "https://auth.example.test/oauth/authorize");
        let params: HashMap<String, String> = query_pairs(query).into_iter().collect();
        assert_eq!(params.len(), 10, "params: {params:?}");
        assert_eq!(params.get("response_type").unwrap(), "code");
        assert_eq!(
            params.get("client_id").unwrap(),
            "app_EMoamEEZ73f0CkXaXp7hrann"
        );
        assert_eq!(
            params.get("redirect_uri").unwrap(),
            "http://localhost:1455/auth/callback"
        );
        assert_eq!(params.get("scope").unwrap(), CODEX_OAUTH_SCOPES);
        assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(params.get("id_token_add_organizations").unwrap(), "true");
        assert_eq!(params.get("codex_cli_simplified_flow").unwrap(), "true");
        assert_eq!(params.get("originator").unwrap(), CODEX_ORIGINATOR);
        let challenge = params.get("code_challenge").unwrap();
        assert_eq!(challenge.len(), 43, "sha256 digest, unpadded base64url");
        assert!(!challenge.contains('+') && !challenge.contains('/') && !challenge.contains('='));
        let state = params.get("state").unwrap();
        assert_eq!(state.len(), 48, "24 bytes hex-encoded");
        assert_eq!(*state, started.state);
    }

    #[tokio::test]
    async fn exchange_carries_codex_headers() {
        let id_token = fake_jwt(claims("me@example.com", "pro", "acc-1"));
        let (token_url, captured) = spawn_mock(token_response(&id_token)).await;
        let sign_in = test_sign_in(token_url);
        let started = sign_in.start().await.unwrap();

        let account = finish(&sign_in, &started).await;
        assert_eq!(account.user, "me@example.com");

        let captured = captured.lock().unwrap();
        assert_eq!(captured.get("originator").unwrap(), "codex_cli_rs");
        assert!(
            captured
                .get("user-agent")
                .unwrap()
                .starts_with("codex_cli_rs/"),
            "user-agent: {:?}",
            captured.get("user-agent")
        );
        assert_eq!(
            captured.get("content-type").unwrap(),
            "application/x-www-form-urlencoded"
        );
        let body = captured.get("body").unwrap();
        assert!(body.contains("code_verifier="), "body: {body}");
        assert!(body.contains("grant_type=authorization_code"));
    }

    #[tokio::test]
    async fn account_identity_comes_from_id_token_claims() {
        let id_token = fake_jwt(claims("me@example.com", "pro", "acc-123"));
        let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
        let sign_in = test_sign_in(token_url);
        let started = sign_in.start().await.unwrap();

        let account = finish(&sign_in, &started).await;
        assert_eq!(account.user, "me@example.com");
        assert_eq!(account.email, "me@example.com");
        assert_eq!(account.plan, "pro");
        assert_eq!(account.account_id, "acc-123");
        assert_eq!(account.account_id_key(), "codex:me@example.com");
        assert_eq!(account.refresh_token, "refresh-secret");
        assert_eq!(account.expires_at_unix, Some(2_000_000_000));
    }

    #[test]
    fn team_plan_is_shown_in_the_name() {
        assert_eq!(
            codex_user("me@example.com", "team"),
            "me@example.com · Team"
        );
        assert_eq!(
            codex_user("me@example.com", "business"),
            "me@example.com · Business"
        );
        assert_eq!(
            codex_user("me@example.com", "enterprise"),
            "me@example.com · Enterprise"
        );
        assert_eq!(codex_user("me@example.com", "edu"), "me@example.com · Edu");
        // Any other plan (including empty) keeps the bare email.
        assert_eq!(codex_user("me@example.com", "pro"), "me@example.com");
        assert_eq!(codex_user("me@example.com", ""), "me@example.com");
    }

    #[tokio::test]
    async fn auth_json_has_the_codex_shape() {
        let id_token = fake_jwt(claims("me@example.com", "pro", "acc-1"));
        let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
        let sign_in = test_sign_in(token_url);
        let started = sign_in.start().await.unwrap();
        let account = finish(&sign_in, &started).await;

        let bytes = account.auth_json();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.len(), 4, "top-level keys: {:?}", obj.keys());
        assert!(obj.get("OPENAI_API_KEY").unwrap().is_null());
        assert_eq!(obj.get("auth_mode").unwrap(), "chatgpt");
        chrono::DateTime::parse_from_rfc3339(obj.get("last_refresh").unwrap().as_str().unwrap())
            .unwrap_or_else(|err| panic!("last_refresh not RFC3339: {err}"));
        let tokens = obj.get("tokens").unwrap().as_object().unwrap();
        assert_eq!(tokens.len(), 4, "token keys: {:?}", tokens.keys());
        assert_eq!(tokens.get("id_token").unwrap(), &id_token);
        assert_eq!(
            tokens.get("access_token").unwrap(),
            &fake_jwt(serde_json::json!({ "exp": 2_000_000_000 }))
        );
        assert_eq!(tokens.get("refresh_token").unwrap(), "refresh-secret");
        assert_eq!(tokens.get("account_id").unwrap(), "acc-1");
        // Keys are sorted: re-serializing the parsed value is byte-identical.
        assert_eq!(serde_json::to_vec(&value).unwrap(), bytes);
    }

    #[tokio::test]
    async fn pasted_callback_finishes_and_mismatch_is_refused() {
        let id_token = fake_jwt(claims("me@example.com", "pro", "acc-1"));
        let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
        let sign_in = test_sign_in(token_url);
        let started = sign_in.start().await.unwrap();

        let wrong = "http://localhost:1455/auth/callback?state=wrong-state&code=auth-code";
        let err = sign_in
            .finish_pasted(wrong, &started.state)
            .await
            .unwrap_err();
        assert!(matches!(err, SignInError::StateMismatch));

        // The mismatch consumed nothing: the correct state still completes.
        let account = finish(&sign_in, &started).await;
        assert_eq!(account.user, "me@example.com");
    }

    #[tokio::test]
    async fn missing_email_is_refused() {
        let id_token = fake_jwt(serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_plan_type": "pro" }
        }));
        let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
        let sign_in = test_sign_in(token_url);
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
    async fn sign_in_writes_back_to_auth_json() {
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
        let access_token = fake_jwt(serde_json::json!({ "exp": 2_000_000_000 }));
        let (token_url, _captured) = spawn_mock(
            serde_json::json!({
                "id_token": id_token,
                "access_token": access_token,
                "refresh_token": "refresh-secret",
            })
            .to_string(),
        )
        .await;
        let sign_in = CodexSignIn::with_endpoints(test_endpoints(token_url))
            .with_home(home.clone())
            .with_loopback_port(0);
        let started = sign_in.start().await.unwrap();
        finish(&sign_in, &started).await;

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&auth_path).unwrap()).unwrap();
        assert_eq!(after["tokens"]["id_token"], id_token);
        assert_eq!(after["tokens"]["access_token"], access_token);
        assert_eq!(after["tokens"]["refresh_token"], "refresh-secret");
        assert_ne!(after["last_refresh"], "old");
        // Everything else is preserved untouched: write_tokens rotates the
        // tokens it owns and never rewrites identity fields.
        assert_eq!(after["tokens"]["account_id"], "old-acc");
        assert_eq!(after["auth_mode"], "chatgpt");
        assert!(after["OPENAI_API_KEY"].is_null());
        assert_eq!(after["custom_field"], "keep-me");
        // The last-good backup exists (magpie's lastgood generation).
        assert!(home.join("auth.json.bak").is_file());
    }

    #[tokio::test]
    async fn busy_port_makes_the_flow_paste_only() {
        // Hold a port with a fast-answering server: the sign-in asks it to
        // leave (`GET /cancel`), gives up after 10 tries and continues
        // paste-only with the URL still produced.
        let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy_port = blocker.local_addr().unwrap().port();
        let app = axum::Router::new().fallback(|| async { StatusCode::NOT_FOUND });
        tokio::spawn(async move {
            let _ = axum::serve(blocker, app).await;
        });

        let sign_in = test_sign_in("http://127.0.0.1:1/oauth/token".to_string())
            .with_loopback_port(busy_port);
        let started = sign_in.start().await.unwrap();

        assert!(started.paste_callback);
        assert_eq!(started.callback_blocked, "busy");
        assert!(
            started
                .url
                .starts_with("https://auth.example.test/oauth/authorize?"),
            "url: {}",
            started.url
        );

        // No listener was held, so `wait` must refuse; the paste path still
        // completes once a state is provided.
        let wait_err = sign_in.wait(Duration::from_millis(100)).await;
        assert!(matches!(wait_err, Err(SignInError::NotStarted)));
    }

    #[tokio::test]
    async fn wait_serves_the_callback_and_completes() {
        let id_token = fake_jwt(claims("me@example.com", "pro", "acc-1"));
        let (token_url, _captured) = spawn_mock(token_response(&id_token)).await;
        let sign_in = test_sign_in(token_url);
        let started = sign_in.start().await.unwrap();
        // The listener took an ephemeral port (tests never touch 1455).
        let port = {
            let listener = sign_in.listener.lock().await;
            listener.as_ref().unwrap().local_addr().unwrap().port()
        };

        let waiter = tokio::spawn(async move { sign_in.wait(Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let client = reqwest::Client::new();
        let response = client
            .get(format!(
                "http://127.0.0.1:{port}/auth/callback?state={}&code=code-1",
                started.state
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let account = waiter.await.unwrap().unwrap();
        assert_eq!(account.email, "me@example.com");
        assert_eq!(account.refresh_token, "refresh-secret");
    }
}
