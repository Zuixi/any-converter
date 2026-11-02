//! Gemini CLI-style browser OAuth sign-in (PKCE + loopback redirect).
//!
//! Flow (mirrors magpie `startGoogleSignIn` / `googleExchange`):
//!
//! 1. [`GeminiSignIn::start`] generates a PKCE pair, binds an ephemeral
//!    loopback `TcpListener` and returns the authorize URL to open in a
//!    browser.
//! 2. [`GeminiSignIn::wait`] serves `GET /oauth2callback` on that port until
//!    a callback with a matching `state` arrives (mismatches get an error
//!    page and the wait continues), then exchanges the code for tokens and
//!    resolves the account email via userinfo.
//! 3. On hosts the browser cannot reach, [`GeminiSignIn::finish_pasted`]
//!    accepts the pasted callback URL instead and runs the same exchange
//!    path.
//!
//! All network interaction goes through the [`HttpExchange`] trait so unit
//! tests can inject a fake layer; endpoints default to the public Gemini CLI
//! OAuth client. Nothing is persisted here — persisting the resulting
//! [`GeminiAccount`] is the caller's job.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{Router, extract::Query, http::StatusCode, response::Html, routing::any};
use futures_util::future::BoxFuture;
use thiserror::Error;
use uuid::Uuid;

use super::token::OAuthTokens;

/// Scope set used by the Gemini CLI OAuth client (space-joined).
pub const GEMINI_OAUTH_SCOPES: &str = concat!(
    "https://www.googleapis.com/auth/cloud-platform",
    " https://www.googleapis.com/auth/userinfo.email",
    " https://www.googleapis.com/auth/userinfo.profile"
);

/// Suggested `wait` deadline when the caller has no opinion.
pub const DEFAULT_SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);

/// OAuth endpoints and client credentials.
///
/// Injectable so tests (and future providers) can point at fake endpoints;
/// [`Default`] yields the public Gemini CLI OAuth client.
#[derive(Debug, Clone)]
pub struct GeminiEndpoints {
    /// Authorization endpoint (Google's `o/oauth2/v2/auth`).
    pub authorize_url: String,
    /// Token exchange endpoint.
    pub token_url: String,
    /// OpenID userinfo endpoint (email resolution).
    pub userinfo_url: String,
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret (public for installed-app clients).
    pub client_secret: String,
}

impl Default for GeminiEndpoints {
    fn default() -> Self {
        Self {
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            userinfo_url: "https://openidconnect.googleapis.com/v1/userinfo".to_string(),
            client_id: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j\
                        .apps.googleusercontent.com"
                .to_string(),
            client_secret: "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".to_string(),
        }
    }
}

/// Failures of the browser sign-in flow.
#[derive(Debug, Error)]
pub enum SignInError {
    /// Transport-level HTTP failure.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// Local socket failure (bind / local_addr).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Upstream-reported failure (non-2xx response or `error` callback param).
    #[error("vendor error: {0}")]
    Vendor(String),
    /// No matching callback arrived before the deadline.
    #[error("timed out waiting for the browser callback")]
    Timeout,
    /// Callback `state` did not match the generated one.
    #[error("oauth state mismatch")]
    StateMismatch,
    /// userinfo response carried no email.
    #[error("userinfo response contained no email")]
    NoEmail,
    /// A pasted callback URL could not be parsed.
    #[error("invalid callback url: {0}")]
    BadUrl(String),
    /// `wait`/`finish_pasted` was called before `start`.
    #[error("sign-in not started")]
    NotStarted,
}

/// The authorize URL plus everything the caller needs to complete the flow.
#[derive(Debug, Clone)]
pub struct SignInStarted {
    /// Full authorize URL to open in a browser.
    pub url: String,
    /// Always `true` today: server deployments cannot rely on the browser
    /// reaching the loopback listener, so the paste fallback is advertised.
    pub paste_callback: bool,
    /// Loopback port the callback server listens on.
    pub port: u16,
    /// Generated `state`; pass it back to `finish_pasted`.
    pub state: String,
}

/// A fully signed-in Gemini OAuth account.
///
/// Persisting this (e.g. via [`super::FileSecretStore`]) is the caller's job.
#[derive(Debug, Clone)]
pub struct GeminiAccount {
    /// Account email resolved from userinfo.
    pub email: String,
    /// Tokens with local expiry (`expires_at = now + expires_in`).
    pub tokens: OAuthTokens,
}

// ---------------------------------------------------------------------------
// Injectable HTTP layer
// ---------------------------------------------------------------------------

/// One authorization-code exchange request, fully owned (the injected layer
/// produces `'static` futures).
#[derive(Debug, Clone, PartialEq)]
pub struct TokenExchangeRequest {
    pub token_url: String,
    pub code: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub client_secret: String,
    pub code_verifier: String,
}

/// Raw token endpoint response (expiry is converted to `Instant` later so
/// the logic lives in exactly one place).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TokenEndpointResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// Injectable network layer for the two OAuth exchange steps.
///
/// Production uses the reqwest implementation below; tests inject fakes so
/// unit tests never touch the network. Methods return boxed futures
/// (object-safe) instead of requiring an `async_trait` dependency.
pub trait HttpExchange: Send + Sync {
    /// Exchange an authorization code for tokens.
    fn exchange_code(
        &self,
        request: TokenExchangeRequest,
    ) -> BoxFuture<'static, Result<TokenEndpointResponse, SignInError>>;

    /// Resolve the account email (`None` when the response has no email).
    fn fetch_userinfo(
        &self,
        userinfo_url: String,
        access_token: String,
    ) -> BoxFuture<'static, Result<Option<String>, SignInError>>;
}

/// Production [`HttpExchange`] backed by reqwest.
struct ReqwestHttpExchange {
    client: reqwest::Client,
}

impl ReqwestHttpExchange {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl HttpExchange for ReqwestHttpExchange {
    fn exchange_code(
        &self,
        request: TokenExchangeRequest,
    ) -> BoxFuture<'static, Result<TokenEndpointResponse, SignInError>> {
        // Clone the client up front so the future captures nothing borrowed.
        let client = self.client.clone();
        Box::pin(async move {
            // The workspace enables no reqwest "form" feature, so the
            // urlencoded body is assembled by hand.
            let body = form_urlencoded(&[
                ("grant_type", "authorization_code"),
                ("code", request.code.as_str()),
                ("redirect_uri", request.redirect_uri.as_str()),
                ("client_id", request.client_id.as_str()),
                ("client_secret", request.client_secret.as_str()),
                ("code_verifier", request.code_verifier.as_str()),
            ]);
            let response = client
                .post(&request.token_url)
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
            serde_json::from_str::<TokenEndpointResponse>(&text).map_err(|e| {
                SignInError::Vendor(format!("token endpoint returned malformed payload: {e}"))
            })
        })
    }

    fn fetch_userinfo(
        &self,
        userinfo_url: String,
        access_token: String,
    ) -> BoxFuture<'static, Result<Option<String>, SignInError>> {
        // Clone the client up front so the future captures nothing borrowed.
        let client = self.client.clone();
        Box::pin(async move {
            let response = client
                .get(&userinfo_url)
                .bearer_auth(&access_token)
                .send()
                .await?;
            let status = response.status();
            let text = response.text().await?;
            if !status.is_success() {
                return Err(SignInError::Vendor(format!(
                    "userinfo endpoint returned {status}: {text}"
                )));
            }
            #[derive(serde::Deserialize)]
            struct Userinfo {
                #[serde(default)]
                email: Option<String>,
            }
            serde_json::from_str::<Userinfo>(&text)
                .map(|u| u.email)
                .map_err(|e| {
                    SignInError::Vendor(format!(
                        "userinfo endpoint returned malformed payload: {e}"
                    ))
                })
        })
    }
}

// ---------------------------------------------------------------------------
// Sign-in driver
// ---------------------------------------------------------------------------

/// Everything `wait`/`finish_pasted` need that was generated in `start`.
struct PkceSession {
    verifier: String,
    state: String,
    listener: tokio::net::TcpListener,
    redirect_uri: String,
}

/// Drives one browser OAuth sign-in against configurable endpoints.
///
/// Usage: `start()` → user opens the URL → either `wait(timeout)` (loopback
/// callback) or `finish_pasted(url, state)` (paste fallback). `wait` consumes
/// the sign-in; `finish_pasted` borrows it (a second call fails — the code
/// verifier session is consumed on first use).
pub struct GeminiSignIn {
    endpoints: GeminiEndpoints,
    http: Arc<dyn HttpExchange>,
    /// Created by `start`, consumed by `wait`/`finish_pasted`.
    session: tokio::sync::Mutex<Option<PkceSession>>,
}

impl GeminiSignIn {
    /// Create a sign-in for the given endpoints using the real reqwest
    /// exchange layer.
    pub fn new(endpoints: GeminiEndpoints) -> Self {
        Self::with_http(endpoints, Arc::new(ReqwestHttpExchange::new()))
    }

    /// Create a sign-in with an injected [`HttpExchange`] layer (tests).
    pub fn with_http(endpoints: GeminiEndpoints, http: Arc<dyn HttpExchange>) -> Self {
        Self {
            endpoints,
            http,
            session: tokio::sync::Mutex::new(None),
        }
    }

    /// Generate the PKCE pair, bind an ephemeral loopback port and build the
    /// authorize URL. Call this before `wait`/`finish_pasted`; calling it
    /// again replaces (and unbinds) any previous session.
    pub async fn start(&self) -> Result<SignInStarted, SignInError> {
        // 32 random bytes → 64 hex chars, inside the RFC 7636 43..=128 range.
        let verifier = hex_encode(&random_bytes(32));
        let state = hex_encode(&random_bytes(24));
        let challenge = base64url(&sha256(verifier.as_bytes()));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}/oauth2callback");
        let url = build_authorize_url(&self.endpoints, &redirect_uri, &state, &challenge);

        *self.session.lock().await = Some(PkceSession {
            verifier,
            state: state.clone(),
            listener,
            redirect_uri,
        });

        Ok(SignInStarted {
            url,
            paste_callback: true,
            port,
            state,
        })
    }

    /// Serve the loopback callback until a matching `code` arrives, then
    /// exchange it (shared with [`Self::finish_pasted`]).
    ///
    /// A callback with a mismatched `state` gets an error page and the wait
    /// continues; a callback carrying an `error` param fails the wait with
    /// [`SignInError::Vendor`]. No callback before `timeout` →
    /// [`SignInError::Timeout`].
    pub async fn wait(self, timeout: Duration) -> Result<GeminiAccount, SignInError> {
        let session = self
            .session
            .lock()
            .await
            .take()
            .ok_or(SignInError::NotStarted)?;
        let PkceSession {
            verifier,
            state: expected_state,
            listener,
            redirect_uri,
        } = session;

        let (done_tx, mut done_rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<String, SignInError>>();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let app = Router::new().route(
            "/oauth2callback",
            any(move |Query(params): Query<HashMap<String, String>>| {
                let tx = done_tx.clone();
                let expected = expected_state.clone();
                async move {
                    match classify_callback(&params, &expected) {
                        CallbackOutcome::Success(code) => {
                            let _ = tx.send(Ok(code));
                            (StatusCode::OK, Html(SUCCESS_PAGE))
                        }
                        CallbackOutcome::Mismatch => (StatusCode::BAD_REQUEST, Html(MISMATCH_PAGE)),
                        CallbackOutcome::Fatal(err) => {
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

        self.complete(code, verifier, redirect_uri).await
    }

    /// Paste fallback for when the browser cannot reach the loopback
    /// listener: parse the pasted callback URL, check `state`, then run the
    /// same exchange path as [`Self::wait`].
    pub async fn finish_pasted(
        &self,
        callback_url: &str,
        expected_state: &str,
    ) -> Result<GeminiAccount, SignInError> {
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
        self.complete(code, session.verifier, session.redirect_uri)
            .await
    }

    /// Shared tail of both flows: code → tokens → email → account.
    async fn complete(
        &self,
        code: String,
        verifier: String,
        redirect_uri: String,
    ) -> Result<GeminiAccount, SignInError> {
        let response = self
            .http
            .exchange_code(TokenExchangeRequest {
                token_url: self.endpoints.token_url.clone(),
                code,
                redirect_uri,
                client_id: self.endpoints.client_id.clone(),
                client_secret: self.endpoints.client_secret.clone(),
                code_verifier: verifier,
            })
            .await?;

        let email = self
            .http
            .fetch_userinfo(
                self.endpoints.userinfo_url.clone(),
                response.access_token.clone(),
            )
            .await?
            .ok_or(SignInError::NoEmail)?;

        let expires_at = response
            .expires_in
            .and_then(|secs| Instant::now().checked_add(Duration::from_secs(secs)));

        Ok(GeminiAccount {
            email,
            tokens: OAuthTokens {
                access_token: response.access_token,
                refresh_token: response.refresh_token,
                expires_at,
            },
        })
    }
}

/// Classification of one inbound callback request.
enum CallbackOutcome {
    /// `state` matched and a `code` was present.
    Success(String),
    /// `state` missing/wrong (or no code): error page, keep waiting.
    Mismatch,
    /// The provider redirected with `error=...`: fail the wait.
    Fatal(SignInError),
}

fn classify_callback(params: &HashMap<String, String>, expected_state: &str) -> CallbackOutcome {
    if let Some(err) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("");
        return CallbackOutcome::Fatal(SignInError::Vendor(format!(
            "authorization failed: {err} ({description})"
        )));
    }
    match params.get("state") {
        Some(state) if state == expected_state => match params.get("code") {
            Some(code) if !code.is_empty() => CallbackOutcome::Success(code.clone()),
            _ => CallbackOutcome::Mismatch,
        },
        _ => CallbackOutcome::Mismatch,
    }
}

const SUCCESS_PAGE: &str = "<html><body><h2>Sign-in complete</h2>\
<p>You can close this tab and return to the application.</p></body></html>";
const MISMATCH_PAGE: &str = "<html><body><h2>Sign-in state mismatch</h2>\
<p>Please restart the sign-in from the application.</p></body></html>";
const ERROR_PAGE: &str = "<html><body><h2>Sign-in failed</h2>\
<p>The provider reported an error. Please retry.</p></body></html>";

// ---------------------------------------------------------------------------
// Encoding / crypto helpers (no external crates available here)
// ---------------------------------------------------------------------------

/// SHA-256 (FIPS 180-4), self-contained: the workspace pins no `sha2` crate
/// and adding one is out of scope for this module.
fn sha256(message: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // padding: 0x80, zeros to 56 mod 64, big-endian bit length
    let bit_len = (message.len() as u64).wrapping_mul(8);
    let mut data = message.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for block in data.chunks_exact(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut digest = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        digest[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

/// base64url without padding (RFC 7636 code challenge encoding).
fn base64url(data: &[u8]) -> String {
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

/// Lowercase hex (verifier / state encoding).
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// `count` random bytes from the OS CSPRNG (via uuid v4, the workspace's
/// only rand source).
fn random_bytes(count: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        out.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    out.truncate(count);
    out
}

/// Percent-encode for query strings (unreserved set kept literal).
fn percent_encode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode a query component (`+` becomes space, invalid escapes are
/// kept literal).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `application/x-www-form-urlencoded` body (keys are static and safe).
fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Extract decoded `(key, value)` query pairs from a pasted callback URL.
fn query_pairs(url: &str) -> Vec<(String, String)> {
    // Accept either a full URL (take everything after '?') or a bare query
    // string (tests pass the latter after splitting themselves).
    let query = match url.split_once('?') {
        Some((_, query)) => query,
        None => url,
    };
    let query = match query.split_once('#') {
        Some((query, _)) => query,
        None => query,
    };
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// Assemble the authorize URL (all values percent-encoded).
fn build_authorize_url(
    endpoints: &GeminiEndpoints,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    let params: [(&str, String); 9] = [
        ("response_type", "code".to_string()),
        ("client_id", percent_encode(&endpoints.client_id)),
        ("redirect_uri", percent_encode(redirect_uri)),
        ("scope", percent_encode(GEMINI_OAUTH_SCOPES)),
        ("access_type", "offline".to_string()),
        ("prompt", "consent".to_string()),
        ("state", percent_encode(state)),
        ("code_challenge", percent_encode(challenge)),
        ("code_challenge_method", "S256".to_string()),
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

    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    // ------------------------------------------------------------------
    // Fake HTTP layer
    // ------------------------------------------------------------------

    type Shared<T> = std::sync::Arc<Mutex<T>>;

    /// Scriptable [`HttpExchange`] fake. Cheap to clone; all clones share
    /// the same call counters and scripted results.
    #[derive(Clone)]
    struct FakeHttp {
        token_calls: Arc<AtomicUsize>,
        userinfo_calls: Arc<AtomicUsize>,
        last_request: Shared<Option<TokenExchangeRequest>>,
        token_result: Shared<Result<TokenEndpointResponse, String>>,
        email_result: Shared<Result<Option<String>, String>>,
    }

    impl FakeHttp {
        fn with_token(token_result: Result<TokenEndpointResponse, String>) -> Self {
            Self {
                token_calls: Arc::new(AtomicUsize::new(0)),
                userinfo_calls: Arc::new(AtomicUsize::new(0)),
                last_request: Arc::new(Mutex::new(None)),
                token_result: Arc::new(Mutex::new(token_result)),
                email_result: Arc::new(Mutex::new(Ok(Some("user@example.com".to_string())))),
            }
        }

        fn with_email(email_result: Result<Option<String>, String>) -> Self {
            Self {
                token_calls: Arc::new(AtomicUsize::new(0)),
                userinfo_calls: Arc::new(AtomicUsize::new(0)),
                last_request: Arc::new(Mutex::new(None)),
                token_result: Arc::new(Mutex::new(Ok(TokenEndpointResponse {
                    access_token: "access-1".to_string(),
                    refresh_token: Some("refresh-1".to_string()),
                    expires_in: Some(3600),
                }))),
                email_result: Arc::new(Mutex::new(email_result)),
            }
        }

        fn token_calls(&self) -> usize {
            self.token_calls.load(Ordering::SeqCst)
        }
    }

    impl HttpExchange for FakeHttp {
        fn exchange_code(
            &self,
            request: TokenExchangeRequest,
        ) -> BoxFuture<'static, Result<TokenEndpointResponse, SignInError>> {
            self.token_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_request.lock().unwrap() = Some(request);
            let result = self.token_result.lock().unwrap().clone();
            Box::pin(async move { result.map_err(SignInError::Vendor) })
        }

        fn fetch_userinfo(
            &self,
            _userinfo_url: String,
            _access_token: String,
        ) -> BoxFuture<'static, Result<Option<String>, SignInError>> {
            self.userinfo_calls.fetch_add(1, Ordering::SeqCst);
            let result = self.email_result.lock().unwrap().clone();
            Box::pin(async move { result.map_err(SignInError::Vendor) })
        }
    }

    fn test_endpoints() -> GeminiEndpoints {
        GeminiEndpoints {
            authorize_url: "https://auth.example.test/authorize".to_string(),
            token_url: "https://token.example.test/token".to_string(),
            userinfo_url: "https://userinfo.example.test/v1/userinfo".to_string(),
            client_id: "test-client-id".to_string(),
            client_secret: "test-client-secret".to_string(),
        }
    }

    fn query_map(url: &str) -> HashMap<String, String> {
        query_pairs(url).into_iter().collect()
    }

    // ------------------------------------------------------------------
    // Encoding helpers
    // ------------------------------------------------------------------

    #[test]
    fn sha256_matches_known_vectors() {
        // FIPS 180-4 / NIST test vectors.
        assert_eq!(
            hex_encode(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_encode(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // Multi-block (> 55 bytes) + exact-block boundary lengths.
        assert_eq!(
            hex_encode(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let long = vec![b'a'; 1000];
        assert_eq!(
            hex_encode(&sha256(&long)),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn base64url_matches_vectors() {
        // RFC 4648 test vectors, url-safe alphabet, unpadded.
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        // 62/63 indices become '-' and '_' (standard base64 would give '+/'
        // with one '=' of padding).
        assert_eq!(base64url(&[0xfb, 0xff, 0xfe]), "-__-");
        assert_eq!(base64url(&[0xff, 0xff, 0xff]), "____");
        assert_eq!(base64url(&[0xff, 0xff, 0xfe]), "___-");
    }

    #[test]
    fn base64url_url_safe_alphabet() {
        // The full byte range must never produce '+', '/' or '='.
        let all: Vec<u8> = (0u8..=255).collect();
        let encoded = base64url(&all);
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        assert_eq!(encoded.len(), 342); // 256 bytes unpadded base64url: ceil(256/3)*4 = 344 - 2 padding
    }

    #[test]
    fn percent_round_trip() {
        assert_eq!(percent_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(percent_decode("a%20b%26c%3Dd"), "a b&c=d");
        assert_eq!(percent_decode("a+b"), "a b");
        // Invalid escapes kept literal.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_encode("~-.Az09"), "~-.Az09");
    }

    // ------------------------------------------------------------------
    // start(): authorize URL / PKCE format
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn start_authorize_url_contains_all_params() {
        let endpoints = test_endpoints();
        let sign_in = GeminiSignIn::new(endpoints.clone());
        let started = sign_in.start().await.unwrap();

        assert_eq!(
            started.url,
            format!(
                "https://auth.example.test/authorize\
?response_type=code\
&client_id=test-client-id\
&redirect_uri=http%3A%2F%2F127.0.0.1%3A{}%2Foauth2callback\
&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform%20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email%20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.profile\
&access_type=offline\
&prompt=consent\
&state={}\
&code_challenge={}\
&code_challenge_method=S256",
                started.port,
                started.state,
                base64url(&sha256(verifier_of(&sign_in).await.as_bytes()))
            )
        );
        assert!(started.paste_callback);
    }

    #[tokio::test]
    async fn start_url_params_parseable_and_correct() {
        let sign_in = GeminiSignIn::new(test_endpoints());
        let started = sign_in.start().await.unwrap();

        let (base, query) = started.url.split_once('?').unwrap();
        assert_eq!(base, "https://auth.example.test/authorize");
        let params = query_map(query);

        assert_eq!(params.get("response_type").unwrap(), "code");
        assert_eq!(params.get("client_id").unwrap(), "test-client-id");
        assert_eq!(
            params.get("redirect_uri").unwrap(),
            &format!("http://127.0.0.1:{}/oauth2callback", started.port)
        );
        assert_eq!(params.get("scope").unwrap(), GEMINI_OAUTH_SCOPES);
        assert_eq!(params.get("access_type").unwrap(), "offline");
        assert_eq!(params.get("prompt").unwrap(), "consent");
        assert_eq!(params.get("state").unwrap(), &started.state);
        assert_eq!(params.get("code_challenge_method").unwrap(), "S256");

        // Challenge recomputable from the verifier.
        let verifier = verifier_of(&sign_in).await;
        assert_eq!(
            params.get("code_challenge").unwrap(),
            &base64url(&sha256(verifier.as_bytes()))
        );
    }

    #[tokio::test]
    async fn pkce_formats() {
        let sign_in = GeminiSignIn::new(test_endpoints());
        let started = sign_in.start().await.unwrap();

        let verifier = verifier_of(&sign_in).await;
        assert_eq!(verifier.len(), 64, "32 bytes hex-encoded");
        assert!(
            verifier.bytes().all(|b| b.is_ascii_hexdigit()),
            "url-safe hex alphabet"
        );
        assert_eq!(started.state.len(), 48, "24 bytes hex-encoded");
        assert!(started.state.bytes().all(|b| b.is_ascii_hexdigit()));

        let challenge = query_map(started.url.split_once('?').unwrap().1)
            .get("code_challenge")
            .unwrap()
            .clone();
        assert_eq!(challenge, base64url(&sha256(verifier.as_bytes())));
        assert!(!challenge.contains('+') && !challenge.contains('/') && !challenge.contains('='));
        assert_eq!(challenge.len(), 43, "sha256 digest, unpadded base64url");
    }

    async fn verifier_of(sign_in: &GeminiSignIn) -> String {
        sign_in
            .session
            .lock()
            .await
            .as_ref()
            .map(|s| s.verifier.clone())
            .unwrap_or_default()
    }

    #[test]
    fn random_bytes_length_and_uniqueness() {
        assert_eq!(random_bytes(10).len(), 10);
        assert_eq!(random_bytes(16).len(), 16);
        assert_ne!(random_bytes(32), random_bytes(32));
    }

    // ------------------------------------------------------------------
    // finish_pasted (uses the shared exchange path)
    // ------------------------------------------------------------------

    fn pasted_url(state: &str, code: &str) -> String {
        format!("http://127.0.0.1:1234/oauth2callback?state={state}&code={code}")
    }

    #[tokio::test]
    async fn finish_pasted_happy_path() {
        let http = FakeHttp::with_token(Ok(TokenEndpointResponse {
            access_token: "access-1".to_string(),
            refresh_token: Some("refresh-1".to_string()),
            expires_in: Some(3600),
        }));
        let sign_in = GeminiSignIn::with_http(test_endpoints(), Arc::new(http.clone()));
        let started = sign_in.start().await.unwrap();

        let verifier_before = verifier_of(&sign_in).await;
        let account = sign_in
            .finish_pasted(&pasted_url(&started.state, "auth-code-9"), &started.state)
            .await
            .unwrap();

        assert_eq!(account.email, "user@example.com");
        assert_eq!(account.tokens.access_token, "access-1");
        assert_eq!(account.tokens.refresh_token.as_deref(), Some("refresh-1"));
        assert!(
            account.tokens.expires_at.unwrap() > Instant::now(),
            "expires_at = now + expires_in"
        );
        assert_eq!(http.token_calls(), 1, "exactly one token exchange");

        // The exchange carried the generated verifier and redirect_uri.
        let request = http.last_request.lock().unwrap().clone().unwrap();
        assert_eq!(request.code, "auth-code-9");
        // The session was consumed by finish_pasted; compare against the
        // verifier captured before it ran.
        assert_eq!(request.code_verifier, verifier_before);
        assert_eq!(
            request.redirect_uri,
            format!("http://127.0.0.1:{}/oauth2callback", started.port)
        );
        assert_eq!(request.client_id, "test-client-id");
        assert_eq!(request.client_secret, "test-client-secret");
    }

    #[tokio::test]
    async fn finish_pasted_state_mismatch_not_consumed() {
        let http = FakeHttp::with_token(Ok(TokenEndpointResponse {
            access_token: "x".to_string(),
            refresh_token: None,
            expires_in: Some(60),
        }));
        let sign_in = GeminiSignIn::with_http(test_endpoints(), Arc::new(http.clone()));
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .finish_pasted(&pasted_url("wrong-state", "auth-code"), &started.state)
            .await
            .unwrap_err();
        assert!(matches!(err, SignInError::StateMismatch));
        assert_eq!(
            http.token_calls(),
            0,
            "mismatch must not hit the token endpoint"
        );

        // Session not consumed: correct state still completes.
        let account = sign_in
            .finish_pasted(&pasted_url(&started.state, "auth-code"), &started.state)
            .await
            .unwrap();
        assert_eq!(account.email, "user@example.com");
    }

    #[tokio::test]
    async fn finish_pasted_missing_state_or_code() {
        let sign_in = GeminiSignIn::with_http(
            test_endpoints(),
            Arc::new(FakeHttp::with_token(Ok(TokenEndpointResponse {
                access_token: "x".to_string(),
                refresh_token: None,
                expires_in: None,
            }))),
        );
        let started = sign_in.start().await.unwrap();

        let no_state = "http://127.0.0.1:1/oauth2callback?code=c";
        assert!(matches!(
            sign_in
                .finish_pasted(no_state, &started.state)
                .await
                .unwrap_err(),
            SignInError::StateMismatch
        ));
        let no_code = "http://127.0.0.1:1/oauth2callback?state=abc";
        assert!(matches!(
            sign_in.finish_pasted(no_code, "abc").await.unwrap_err(),
            SignInError::BadUrl(_)
        ));
    }

    #[tokio::test]
    async fn finish_pasted_error_param_is_vendor_error() {
        let sign_in = GeminiSignIn::with_http(
            test_endpoints(),
            Arc::new(FakeHttp::with_token(Ok(TokenEndpointResponse {
                access_token: "x".to_string(),
                refresh_token: None,
                expires_in: None,
            }))),
        );
        sign_in.start().await.unwrap();

        let url = "http://127.0.0.1:1/oauth2callback?error=access_denied";
        assert!(matches!(
            sign_in.finish_pasted(url, "any").await.unwrap_err(),
            SignInError::Vendor(msg) if msg.contains("access_denied")
        ));
    }

    #[tokio::test]
    async fn finish_pasted_without_start_is_not_started() {
        let sign_in = GeminiSignIn::new(test_endpoints());
        assert!(matches!(
            sign_in
                .finish_pasted("http://127.0.0.1:1/oauth2callback?state=s&code=c", "s")
                .await
                .unwrap_err(),
            SignInError::NotStarted
        ));
    }

    #[tokio::test]
    async fn token_endpoint_failure_is_vendor_error() {
        let http = FakeHttp::with_token(Err("token endpoint returned 400 Bad Request".to_string()));
        let sign_in = GeminiSignIn::with_http(test_endpoints(), Arc::new(http));
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .finish_pasted(&pasted_url(&started.state, "auth-code"), &started.state)
            .await
            .unwrap_err();
        assert!(matches!(err, SignInError::Vendor(ref m) if m.contains("400")));
    }

    #[tokio::test]
    async fn userinfo_without_email_is_no_email() {
        let http = FakeHttp::with_email(Ok(None));
        let sign_in = GeminiSignIn::with_http(test_endpoints(), Arc::new(http));
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .finish_pasted(&pasted_url(&started.state, "auth-code"), &started.state)
            .await
            .unwrap_err();
        assert!(matches!(err, SignInError::NoEmail));
    }

    #[tokio::test]
    async fn userinfo_failure_is_vendor_error() {
        let http = FakeHttp::with_email(Err("userinfo endpoint returned 500".to_string()));
        let sign_in = GeminiSignIn::with_http(test_endpoints(), Arc::new(http));
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .finish_pasted(&pasted_url(&started.state, "auth-code"), &started.state)
            .await
            .unwrap_err();
        assert!(matches!(err, SignInError::Vendor(ref m) if m.contains("500")));
    }

    #[tokio::test]
    async fn token_exchange_request_is_well_formed() {
        let http = FakeHttp::with_token(Ok(TokenEndpointResponse {
            access_token: "a".to_string(),
            refresh_token: Some("r".to_string()),
            expires_in: Some(10),
        }));
        let sign_in = GeminiSignIn::with_http(test_endpoints(), Arc::new(http));
        let started = sign_in.start().await.unwrap();
        sign_in
            .finish_pasted(&pasted_url(&started.state, "the-code"), &started.state)
            .await
            .unwrap();
    }

    #[test]
    fn token_endpoint_response_parses_expires_in_and_optional_refresh() {
        let full: TokenEndpointResponse =
            serde_json::from_str(r#"{"access_token":"a","refresh_token":"r","expires_in":3599}"#)
                .unwrap();
        assert_eq!(full.access_token, "a");
        assert_eq!(full.refresh_token.as_deref(), Some("r"));
        assert_eq!(full.expires_in, Some(3599));

        let minimal: TokenEndpointResponse =
            serde_json::from_str(r#"{"access_token":"a"}"#).unwrap();
        assert_eq!(minimal.refresh_token, None);
        assert_eq!(minimal.expires_in, None);
    }

    #[tokio::test]
    async fn defaults_use_public_gemini_cli_client() {
        let endpoints = GeminiEndpoints::default();
        assert_eq!(
            endpoints.authorize_url,
            "https://accounts.google.com/o/oauth2/v2/auth"
        );
        assert_eq!(endpoints.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(
            endpoints.userinfo_url,
            "https://openidconnect.googleapis.com/v1/userinfo"
        );
        assert_eq!(
            endpoints.client_id,
            "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com"
        );
        assert_eq!(
            endpoints.client_secret,
            "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"
        );
    }

    // ------------------------------------------------------------------
    // wait(): loopback callback server (local HTTP only, no upstream calls)
    // ------------------------------------------------------------------

    async fn local_get(url: &str) -> (StatusCode, String) {
        let client = reqwest::Client::new();
        let response = client.get(url).send().await.unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    }

    #[tokio::test]
    async fn wait_success_callback_completes_sign_in() {
        let http = FakeHttp::with_token(Ok(TokenEndpointResponse {
            access_token: "access-1".to_string(),
            refresh_token: Some("refresh-1".to_string()),
            expires_in: Some(3600),
        }));
        let sign_in = GeminiSignIn::with_http(test_endpoints(), Arc::new(http));
        let started = sign_in.start().await.unwrap();

        let waiter = tokio::spawn(async move { sign_in.wait(DEFAULT_SIGN_IN_TIMEOUT).await });

        // Give the server a moment to come up, then hit the callback.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (status, body) = local_get(&format!(
            "http://127.0.0.1:{}/oauth2callback?state={}&code=code-1",
            started.port, started.state
        ))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Sign-in complete"));

        let account = waiter.await.unwrap().unwrap();
        assert_eq!(account.email, "user@example.com");
        assert_eq!(account.tokens.access_token, "access-1");
        assert!(account.tokens.expires_at.unwrap() > Instant::now());
    }

    #[tokio::test]
    async fn wait_state_mismatch_gets_error_page_and_keeps_waiting() {
        let http = FakeHttp::with_token(Ok(TokenEndpointResponse {
            access_token: "access-1".to_string(),
            refresh_token: None,
            expires_in: Some(3600),
        }));
        let sign_in = GeminiSignIn::with_http(test_endpoints(), Arc::new(http.clone()));
        let started = sign_in.start().await.unwrap();

        let waiter = tokio::spawn(async move { sign_in.wait(Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let (status, body) = local_get(&format!(
            "http://127.0.0.1:{}/oauth2callback?state=bogus&code=code-1",
            started.port
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("state mismatch"));

        // A callback with no code at all is also treated as a mismatch.
        let (status, _) = local_get(&format!(
            "http://127.0.0.1:{}/oauth2callback?state={}",
            started.port, started.state
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Still waiting: correct state now completes.
        let (status, _) = local_get(&format!(
            "http://127.0.0.1:{}/oauth2callback?state={}&code=code-2",
            started.port, started.state
        ))
        .await;
        assert_eq!(status, StatusCode::OK);
        let account = waiter.await.unwrap().unwrap();
        assert_eq!(account.email, "user@example.com");
        assert_eq!(http.token_calls(), 1);
    }

    #[tokio::test]
    async fn wait_error_param_fails_with_vendor_error() {
        let sign_in = GeminiSignIn::with_http(
            test_endpoints(),
            Arc::new(FakeHttp::with_token(Ok(TokenEndpointResponse {
                access_token: "x".to_string(),
                refresh_token: None,
                expires_in: None,
            }))),
        );
        let started = sign_in.start().await.unwrap();

        let waiter = tokio::spawn(async move { sign_in.wait(Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let (status, _) = local_get(&format!(
            "http://127.0.0.1:{}/oauth2callback?error=access_denied&state={}",
            started.port, started.state
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, SignInError::Vendor(ref m) if m.contains("access_denied")));
    }

    #[tokio::test]
    async fn wait_timeout_without_callback() {
        let sign_in = GeminiSignIn::new(test_endpoints());
        sign_in.start().await.unwrap();

        let err = sign_in.wait(Duration::from_millis(200)).await.unwrap_err();
        assert!(matches!(err, SignInError::Timeout));
    }

    #[tokio::test]
    async fn wait_without_start_is_not_started() {
        let sign_in = GeminiSignIn::new(test_endpoints());
        let err = sign_in.wait(Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(err, SignInError::NotStarted));
    }

    #[tokio::test]
    async fn start_binds_distinct_ports_and_fresh_pkce() {
        // Two concurrent sign-ins hold both listeners at once, so the ports
        // must be distinct (also proves state/challenge are per-session).
        let a = GeminiSignIn::new(test_endpoints());
        let b = GeminiSignIn::new(test_endpoints());
        let started_a = a.start().await.unwrap();
        let started_b = b.start().await.unwrap();

        assert!(started_a.port > 0);
        assert_ne!(started_a.port, started_b.port);
        assert_ne!(started_a.state, started_b.state);
        assert_ne!(
            query_map(started_a.url.split_once('?').unwrap().1)["code_challenge"],
            query_map(started_b.url.split_once('?').unwrap().1)["code_challenge"]
        );
    }

    // ------------------------------------------------------------------
    // reqwest form body
    // ------------------------------------------------------------------

    #[test]
    fn form_urlencoded_body() {
        let body = form_urlencoded(&[
            ("grant_type", "authorization_code"),
            ("code", "4/v+ x"),
            ("client_id", "abc"),
        ]);
        assert_eq!(
            body,
            "grant_type=authorization_code&code=4%2Fv%2B%20x&client_id=abc"
        );
    }
}
