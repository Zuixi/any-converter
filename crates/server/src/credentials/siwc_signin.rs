//! Sign in with ChatGPT: a ChatGPT plan used through OpenAI's own API.
//!
//! Ported from magpie's `internal/provider/chatgpt_api.go`. OpenAI's page
//! registers the app as it signs in (`dynamic_agent_client`, no secret), so
//! the browser comes back with the client id it issued beside the code; the
//! code, every refresh and the sign-out all go with that id. `ext_agent_host_id`
//! names this machine and is the same every time.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::gemini::{
    ERROR_PAGE, MISMATCH_PAGE, SUCCESS_PAGE, SignInError, base64url, base64url_decode,
    form_urlencoded, percent_encode, query_pairs, random_bytes, sha256,
};
use super::siwc_store::{self, SiwcMeta};
use super::store::SecretStore;

/// What a token answer carries (magpie `siwcTokenReply`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SiwcTokens {
    pub access: String,
    pub refresh: String,
    pub id_token: String,
    pub expires_in: i64,
    pub scope: String,
}

/// A token endpoint's refusal, with its OAuth code (magpie `siwcTokenError`).
#[derive(Debug, Clone, PartialEq)]
pub struct SiwcTokenError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for SiwcTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.code.is_empty(), self.message.is_empty()) {
            (false, false) => write!(f, "{}: {}", self.code, self.message),
            (false, true) => write!(f, "{}", self.code),
            (true, false) => write!(f, "{}", self.message),
            (true, true) => write!(f, "HTTP {}", self.status),
        }
    }
}

/// Why a stored sign-in can no longer be renewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiwcError {
    /// OpenAI will never take this refresh token again: sign in again.
    SignInExpired,
    /// OpenAI no longer knows the client the sign-in registered.
    ClientUnknown,
    /// Anything else, with why.
    Other(String),
}

impl std::fmt::Display for SiwcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SiwcError::SignInExpired => {
                write!(f, "the ChatGPT sign-in has expired — sign in again")
            }
            SiwcError::ClientUnknown => write!(
                f,
                "OpenAI no longer knows the client registered for this account — sign in again"
            ),
            SiwcError::Other(why) => f.write_str(why),
        }
    }
}

/// The refusals of a refresh token that will never work again (magpie
/// `siwcDead`).
const SIWC_DEAD: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// Endpoints and the app's constants; a struct so tests can point them at a
/// local server (magpie's `siwc*` vars).
#[derive(Debug, Clone)]
pub struct SiwcEndpoints {
    pub issuer: String,
    pub authorize_url: String,
    pub token_url: String,
    /// Where the account's models are listed, and what the token is for.
    pub api: String,
    /// This app's name, told to OpenAI on the way (`agent_name_hint`).
    pub agent_name: String,
}

impl Default for SiwcEndpoints {
    fn default() -> Self {
        Self {
            issuer: "https://auth.openai.com".to_string(),
            authorize_url: "https://auth.openai.com/api/accounts/authorize".to_string(),
            token_url: "https://auth.openai.com/api/accounts/oauth/token".to_string(),
            api: "https://api.openai.com/v1".to_string(),
            agent_name: "any-converter".to_string(),
        }
    }
}

/// The app registers itself on the first sign-in (magpie `siwcDynamicClient`).
pub const SIWC_DYNAMIC_CLIENT: &str = "dynamic_agent_client";
/// The scope that lets the token be used against api.openai.com itself.
pub const SIWC_DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";
/// What the sign-in asks for (magpie `siwcScopes`).
pub const SIWC_SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// How long before it expires a token is refreshed.
pub const SIWC_MARGIN: Duration = Duration::from_secs(180);
/// Where OpenAI says how much is left, for the card's "usage" link.
pub const SIWC_USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// The callback the browser is sent back to first; another port when that
/// one is busy (only the port of a loopback address may vary, magpie
/// `siwcCallbackAddr`).
pub const SIWC_CALLBACK_PORT: u16 = 1455;

/// A sign-in in flight: what the browser needs and what finishing needs.
#[derive(Debug, Clone)]
pub struct SiwcStarted {
    pub url: String,
    pub state: String,
    pub nonce: String,
    pub port: u16,
    /// The PKCE verifier; only this side ever sees it.
    pub verifier: String,
    /// The page may not reach this machine; the address it ends on finishes
    /// the sign-in when pasted.
    pub paste_callback: bool,
}

/// A finished sign-in.
#[derive(Debug, Clone, PartialEq)]
pub struct SiwcAccount {
    pub user: String,
    pub plan: String,
    pub access: String,
    pub refresh: String,
    pub id_token: String,
    pub client_id: String,
    pub host_id: String,
    pub scopes: Vec<String>,
    pub expires_at_unix: i64,
}

impl SiwcAccount {
    /// The account id a store keys it by (`chatgpt-api:<email>`).
    pub fn account_id(&self) -> String {
        format!("chatgpt-api:{}", self.user)
    }
}

/// One HTTP answer, as the injected layer returns it.
#[derive(Debug, Clone, PartialEq)]
pub struct SiwcReply {
    pub status: u16,
    pub body: String,
}

/// The HTTP surface the flow needs; `ReqwestSiwc` is the real one, tests
/// inject their own.
pub trait SiwcHttp: Send + Sync {
    fn get(
        &self,
        url: String,
        headers: Vec<(String, String)>,
    ) -> BoxFuture<'static, Result<SiwcReply, SignInError>>;
    fn post(
        &self,
        url: String,
        headers: Vec<(String, String)>,
        body: String,
    ) -> BoxFuture<'static, Result<SiwcReply, SignInError>>;
}

/// The real transport.
#[derive(Debug, Clone)]
pub struct ReqwestSiwc {
    client: reqwest::Client,
}

impl ReqwestSiwc {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl SiwcHttp for ReqwestSiwc {
    fn get(
        &self,
        url: String,
        headers: Vec<(String, String)>,
    ) -> BoxFuture<'static, Result<SiwcReply, SignInError>> {
        let client = self.client.clone();
        Box::pin(async move {
            let mut request = client.get(&url);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = request.send().await?;
            let status = response.status().as_u16();
            Ok(SiwcReply {
                status,
                body: response.text().await?,
            })
        })
    }

    fn post(
        &self,
        url: String,
        headers: Vec<(String, String)>,
        body: String,
    ) -> BoxFuture<'static, Result<SiwcReply, SignInError>> {
        let client = self.client.clone();
        Box::pin(async move {
            let mut request = client.post(&url).body(body);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = request.send().await?;
            let status = response.status().as_u16();
            Ok(SiwcReply {
                status,
                body: response.text().await?,
            })
        })
    }
}

/// The sign-in itself.
pub struct SiwcSignIn {
    endpoints: SiwcEndpoints,
    http: std::sync::Arc<dyn SiwcHttp>,
    meta_path: PathBuf,
    host_path: PathBuf,
    /// Where the tokens are kept, for `refresh`/`revoke` (tests inject).
    secrets_path: PathBuf,
    /// Fixed for tests; otherwise the flow picks 1455 or a free port.
    port: Option<u16>,
    host_id: Option<String>,
}

impl SiwcSignIn {
    pub fn new(client: reqwest::Client) -> Self {
        Self::with_http(
            SiwcEndpoints::default(),
            std::sync::Arc::new(ReqwestSiwc::new(client)),
        )
    }

    pub fn with_http(endpoints: SiwcEndpoints, http: std::sync::Arc<dyn SiwcHttp>) -> Self {
        Self {
            endpoints,
            http,
            meta_path: siwc_store::default_path(),
            host_path: siwc_store::host_id_path(),
            secrets_path: crate::config::secrets_path(),
            port: None,
            host_id: None,
        }
    }

    pub fn with_meta_path(mut self, path: PathBuf) -> Self {
        self.meta_path = path;
        self
    }

    /// Where the tokens are kept (defaults to the app's secrets file).
    pub fn with_secrets_path(mut self, path: PathBuf) -> Self {
        self.secrets_path = path;
        self
    }

    pub fn with_host_id(mut self, id: impl Into<String>) -> Self {
        self.host_id = Some(id.into());
        self
    }

    /// Bind a specific port (`0` lets the OS pick), for tests.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    fn host_id(&self) -> Result<String, SignInError> {
        if let Some(id) = &self.host_id {
            return Ok(id.clone());
        }
        siwc_store::host_id(&self.host_path).map_err(SignInError::Vendor)
    }

    /// Begin a sign-in: the page to open, and what finishing it needs.
    pub fn start(&self) -> Result<SiwcStarted, SignInError> {
        let host_id = self.host_id()?;
        let verifier = base64url(&random_bytes(32));
        let challenge = base64url(&sha256(verifier.as_bytes()));
        let state = base64url(&random_bytes(24));
        let nonce = base64url(&random_bytes(24));
        let (port, bound) = self.bind()?;
        let redirect = format!("http://127.0.0.1:{port}/auth/callback");

        let mut query = vec![
            ("client_id", SIWC_DYNAMIC_CLIENT.to_string()),
            ("agent_name_hint", self.endpoints.agent_name.clone()),
            ("ext_agent_host_id", host_id),
            ("response_type", "code".to_string()),
            ("redirect_uri", redirect),
            ("resource", self.endpoints.api.clone()),
            ("scope", SIWC_SCOPES.to_string()),
            ("state", state.clone()),
            ("nonce", nonce.clone()),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256".to_string()),
        ];
        let encoded = query
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        query.clear();

        Ok(SiwcStarted {
            url: format!("{}?{encoded}", self.endpoints.authorize_url),
            state,
            nonce,
            port,
            verifier,
            paste_callback: bound,
        })
    }

    /// The port the callback listens on: 1455 when it can be had, else a
    /// free one (magpie `listenSIWCCallback`). The boolean says whether the
    /// port still has to be bound by the caller's listener, which every
    /// caller here does — so it is always true: the page may not reach this
    /// machine and the pasted address finishes the flow instead.
    fn bind(&self) -> Result<(u16, bool), SignInError> {
        if let Some(port) = self.port {
            return Ok((port, true));
        }
        match std::net::TcpListener::bind(("127.0.0.1", SIWC_CALLBACK_PORT)) {
            Ok(listener) => {
                let port = listener
                    .local_addr()
                    .map(|a| a.port())
                    .unwrap_or(SIWC_CALLBACK_PORT);
                drop(listener);
                Ok((port, true))
            }
            Err(_) => {
                let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
                    .map_err(|e| SignInError::Vendor(format!("no loopback port: {e}")))?;
                let port = listener
                    .local_addr()
                    .map_err(|e| SignInError::Vendor(e.to_string()))?
                    .port();
                drop(listener);
                Ok((port, true))
            }
        }
    }

    /// Finish with what the callback carried. `client_id` is the one OpenAI
    /// issued while registering the app — without it there is nothing to
    /// talk to OpenAI with.
    pub async fn finish(
        &self,
        code: &str,
        client_id: &str,
        started: &SiwcStarted,
    ) -> Result<SiwcAccount, SignInError> {
        let client_id = client_id.trim();
        if client_id.is_empty() {
            return Err(SignInError::Vendor(format!(
                "OpenAI didn't finish registering {} (no client id came back); start it again",
                self.endpoints.agent_name
            )));
        }
        let redirect = format!("http://127.0.0.1:{}/auth/callback", started.port);
        let form = form_urlencoded(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", &started.verifier),
            ("redirect_uri", &redirect),
            ("resource", &self.endpoints.api),
        ]);
        let tokens = self.post_token(&form).await?;
        let account = self.account_of_tokens(&tokens, client_id, &started.nonce)?;
        siwc_store::put(
            &self.meta_path,
            &account.account_id(),
            &SiwcMeta {
                client_id: account.client_id.clone(),
                host_id: account.host_id.clone(),
                id_token: account.id_token.clone(),
                scopes: account.scopes.clone(),
            },
        )
        .map_err(SignInError::Vendor)?;
        Ok(account)
    }

    /// Finish from the address the browser ended on.
    pub async fn finish_pasted(
        &self,
        url: &str,
        started: &SiwcStarted,
    ) -> Result<SiwcAccount, SignInError> {
        let pairs = query_pairs(url);
        let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        if let Some(error) = get("error") {
            let description = get("error_description").unwrap_or_default();
            return Err(SignInError::Vendor(format!(
                "authorization failed: {error} ({description})"
            )));
        }
        match get("state") {
            Some(state) if state == started.state => {}
            _ => return Err(SignInError::StateMismatch),
        }
        let code = get("code")
            .ok_or_else(|| SignInError::Vendor("that address carries no code".to_string()))?;
        let client_id = get("client_id").unwrap_or_default();
        self.finish(&code, &client_id, started).await
    }

    /// POST the token endpoint and read its answer (magpie `siwcPostToken`).
    async fn post_token(&self, form: &str) -> Result<SiwcTokens, SignInError> {
        let reply = self
            .http
            .post(
                self.endpoints.token_url.clone(),
                vec![
                    (
                        "Content-Type".to_string(),
                        "application/x-www-form-urlencoded".to_string(),
                    ),
                    ("Accept".to_string(), "application/json".to_string()),
                ],
                form.to_string(),
            )
            .await?;
        if reply.status != 200 {
            return Err(SignInError::Vendor(token_refusal(&reply).to_string()));
        }
        parse_tokens(&reply.body)
    }

    fn account_of_tokens(
        &self,
        tokens: &SiwcTokens,
        client_id: &str,
        nonce: &str,
    ) -> Result<SiwcAccount, SignInError> {
        let account = account_from(tokens, client_id, nonce, &self.host_id()?)?;
        Ok(account)
    }

    /// Renew a stored account (magpie `siwcFresh`): a dead refresh token is
    /// [`SiwcError::SignInExpired`], an unknown client
    /// [`SiwcError::ClientUnknown`], and any other failure keeps the token
    /// in hand while it is still good.
    pub async fn refresh(&self, account_id: &str, force: bool) -> Result<SiwcTokens, SiwcError> {
        let meta = siwc_store::get(&self.meta_path, account_id)
            .ok_or_else(|| SiwcError::Other(format!("no ChatGPT API metadata for {account_id}")))?;
        let stored = crate::credentials::FileSecretStore::new(self.secrets_path.clone())
            .get(account_id)
            .ok()
            .flatten();
        let (access, refresh, expires_at) = match stored {
            Some(crate::credentials::Credential::OAuth {
                access_token,
                refresh_token,
                expires_at_unix,
                ..
            }) => (
                access_token,
                refresh_token.unwrap_or_default(),
                expires_at_unix.unwrap_or(0),
            ),
            _ => return Err(SiwcError::Other(format!("no sign-in for {account_id}"))),
        };
        let now = unix_now();
        if !force && expires_at - now > SIWC_MARGIN.as_secs() as i64 {
            return Ok(SiwcTokens {
                access,
                refresh,
                id_token: meta.id_token,
                expires_in: expires_at - now,
                scope: meta.scopes.join(" "),
            });
        }

        let form = form_urlencoded(&[
            ("grant_type", "refresh_token"),
            ("client_id", &meta.client_id),
            ("refresh_token", &refresh),
            ("resource", &self.endpoints.api),
        ]);
        let reply = self
            .http
            .post(
                self.endpoints.token_url.clone(),
                vec![
                    (
                        "Content-Type".to_string(),
                        "application/x-www-form-urlencoded".to_string(),
                    ),
                    ("Accept".to_string(), "application/json".to_string()),
                ],
                form,
            )
            .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(err) => {
                // A hiccup — the endpoint unreachable, no answer — is not a
                // refusal: while the token in hand has not expired it still
                // does, and the request goes on with it (magpie siwcFresh).
                if !force && expires_at > unix_now() {
                    return Ok(SiwcTokens {
                        access,
                        refresh,
                        id_token: meta.id_token,
                        expires_in: expires_at - unix_now(),
                        scope: meta.scopes.join(" "),
                    });
                }
                return Err(SiwcError::Other(format!("ChatGPT token refresh: {err}")));
            }
        };
        if reply.status != 200 {
            let refusal = token_refusal(&reply);
            return Err(classify_refresh(&refusal));
        }
        let tokens = parse_tokens(&reply.body).map_err(|e| SiwcError::Other(e.to_string()))?;
        if !tokens
            .scope
            .split_ascii_whitespace()
            .any(|s| s == SIWC_DIRECT_SCOPE)
        {
            return Err(SiwcError::Other(format!(
                "OpenAI didn't grant the use of the subscription through its API ({SIWC_DIRECT_SCOPE})"
            )));
        }
        Ok(tokens)
    }

    /// Sign out: OpenAI revokes the refresh token at the endpoint it
    /// publishes (magpie `siwcRevoke`). `None` when it publishes none.
    pub async fn revoke(&self, account_id: &str) -> Result<bool, SiwcError> {
        let meta = siwc_store::get(&self.meta_path, account_id)
            .ok_or_else(|| SiwcError::Other(format!("no ChatGPT API metadata for {account_id}")))?;
        let stored = crate::credentials::FileSecretStore::new(self.secrets_path.clone())
            .get(account_id)
            .ok()
            .flatten();
        let refresh = match stored {
            Some(crate::credentials::Credential::OAuth { refresh_token, .. }) => {
                refresh_token.unwrap_or_default()
            }
            _ => return Err(SiwcError::Other(format!("no sign-in for {account_id}"))),
        };

        let cookie = format!(
            "{}/.well-known/openid-configuration",
            self.endpoints.issuer.trim_end_matches('/')
        );
        let reply = self
            .http
            .get(cookie, Vec::new())
            .await
            .map_err(|e| SiwcError::Other(e.to_string()))?;
        let endpoint = serde_json::from_str::<serde_json::Value>(&reply.body)
            .ok()
            .and_then(|v| {
                v.get("revocation_endpoint")
                    .and_then(|e| e.as_str())
                    .map(str::to_string)
            });
        let Some(endpoint) = endpoint.filter(|e| !e.is_empty()) else {
            return Ok(false);
        };
        let form = form_urlencoded(&[
            ("token", &refresh),
            ("token_type_hint", "refresh_token"),
            ("client_id", &meta.client_id),
        ]);
        let reply = self
            .http
            .post(
                endpoint,
                vec![(
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                )],
                form,
            )
            .await
            .map_err(|e| SiwcError::Other(e.to_string()))?;
        if reply.status != 200 {
            return Err(SiwcError::Other(format!(
                "ChatGPT sign-out: {}",
                reply.status
            )));
        }
        Ok(true)
    }

    /// The account's models: OpenAI's own list for its token
    /// (magpie `siwcFetchModels`).
    pub async fn models(&self, account_id: &str, access: &str) -> Result<Vec<String>, SiwcError> {
        let _ = account_id;
        let url = format!("{}/models", self.endpoints.api.trim_end_matches('/'));
        let reply = self
            .http
            .get(
                url,
                vec![("Authorization".to_string(), format!("Bearer {access}"))],
            )
            .await
            .map_err(|e| SiwcError::Other(e.to_string()))?;
        if reply.status != 200 {
            return Err(SiwcError::Other(format!("models: {}", reply.status)));
        }
        let value: serde_json::Value =
            serde_json::from_str(&reply.body).map_err(|e| SiwcError::Other(e.to_string()))?;
        Ok(value
            .get("data")
            .and_then(|d| d.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.get("id").and_then(|id| id.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Parse a token endpoint's answer (magpie `siwcFromToken`'s checks).
fn parse_tokens(body: &str) -> Result<SiwcTokens, SignInError> {
    #[derive(serde::Deserialize)]
    struct Reply {
        #[serde(default)]
        access_token: String,
        #[serde(default)]
        refresh_token: String,
        #[serde(default)]
        id_token: String,
        #[serde(default)]
        expires_in: f64,
        #[serde(default)]
        scope: String,
    }
    let reply: Reply = serde_json::from_str(body)
        .map_err(|e| SignInError::Vendor(format!("unreadable token answer: {e}")))?;
    if reply.access_token.is_empty() || reply.refresh_token.is_empty() {
        return Err(SignInError::Vendor("OpenAI sent no token".to_string()));
    }
    if reply.expires_in <= 0.0 {
        return Err(SignInError::Vendor(
            "OpenAI's token has no lifetime".to_string(),
        ));
    }
    if !reply
        .scope
        .split_ascii_whitespace()
        .any(|s| s == SIWC_DIRECT_SCOPE)
    {
        return Err(SignInError::Vendor(format!(
            "OpenAI didn't grant the use of the subscription through its API ({SIWC_DIRECT_SCOPE}): \
             this ChatGPT account may not be eligible"
        )));
    }
    Ok(SiwcTokens {
        access: reply.access_token,
        refresh: reply.refresh_token,
        id_token: reply.id_token,
        expires_in: reply.expires_in as i64,
        scope: reply.scope,
    })
}

/// A refused token answer, as the error says (magpie `siwcTokenError`).
fn token_refusal(reply: &SiwcReply) -> SiwcTokenError {
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap_or_default();
    let mut code = String::new();
    let mut message = String::new();
    match value.get("error") {
        Some(serde_json::Value::String(text)) => code = text.clone(),
        Some(serde_json::Value::Object(row)) => {
            code = row
                .get("code")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            message = row
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
        }
        _ => {}
    }
    if let Some(desc) = value.get("error_description").and_then(|d| d.as_str()) {
        if message.is_empty() {
            message = desc.to_string();
        }
    }
    if let Some(c) = value.get("code").and_then(|c| c.as_str()) {
        if code.is_empty() {
            code = c.to_string();
        }
    }
    if code.is_empty() && message.is_empty() {
        message = format!("HTTP {}", reply.status);
    }
    SiwcTokenError {
        status: reply.status,
        code,
        message,
    }
}

/// Which refresh refusals mean "sign in again" (magpie `siwcDead` and
/// `invalid_client`).
fn classify_refresh(refusal: &SiwcTokenError) -> SiwcError {
    if SIWC_DEAD.contains(&refusal.code.as_str()) {
        return SiwcError::SignInExpired;
    }
    if refusal.code == "invalid_client" {
        return SiwcError::ClientUnknown;
    }
    SiwcError::Other(format!(
        "ChatGPT token refresh: {}{}",
        refusal.code,
        if refusal.message.is_empty() {
            String::new()
        } else {
            format!(": {}", refusal.message)
        }
    ))
}

/// The account a token answer describes (magpie `siwcSignedIn` +
/// `siwcCheckID`).
fn account_from(
    tokens: &SiwcTokens,
    client_id: &str,
    nonce: &str,
    host_id: &str,
) -> Result<SiwcAccount, SignInError> {
    if tokens.id_token.is_empty() {
        return Err(SignInError::Vendor(
            "ChatGPT sign-in: OpenAI sent no ID token".to_string(),
        ));
    }
    let claims = jwt_claims(&tokens.id_token)
        .ok_or_else(|| SignInError::Vendor("OpenAI sent an unreadable ID token".to_string()))?;
    check_id_token(&claims, client_id, nonce)?;
    let email = claim_string(&claims, &["email"]).unwrap_or_default();
    let subject = claim_string(&claims, &["sub"]).unwrap_or_default();
    let user = if email.is_empty() { subject } else { email };
    if user.is_empty() {
        return Err(SignInError::Vendor(
            "ChatGPT sign-in: OpenAI didn't say which account signed in".to_string(),
        ));
    }
    let plan = plan_of(&claims, &tokens.access);
    let now = unix_now();
    Ok(SiwcAccount {
        user,
        plan,
        access: tokens.access.clone(),
        refresh: tokens.refresh.clone(),
        id_token: tokens.id_token.clone(),
        client_id: client_id.to_string(),
        host_id: host_id.to_string(),
        scopes: tokens
            .scope
            .split_ascii_whitespace()
            .map(str::to_string)
            .collect(),
        expires_at_unix: now + tokens.expires_in,
    })
}

/// The ID token must be this sign-in's, for this app, from OpenAI
/// (magpie `siwcCheckID`: the signature is not checked, the claims are).
fn check_id_token(
    claims: &serde_json::Value,
    client_id: &str,
    nonce: &str,
) -> Result<(), SignInError> {
    let issuer = claim_string(claims, &["iss"]).unwrap_or_default();
    if issuer.trim_end_matches('/') != "https://auth.openai.com".trim_end_matches('/') {
        return Err(SignInError::Vendor(format!(
            "the ID token is from {issuer:?}, not OpenAI"
        )));
    }
    let audience_ok = match claims.get("aud") {
        Some(serde_json::Value::String(one)) => one == client_id,
        Some(serde_json::Value::Array(all)) => all.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience_ok {
        return Err(SignInError::Vendor(
            "the ID token is for another app".to_string(),
        ));
    }
    if claim_string(claims, &["nonce"]).unwrap_or_default() != nonce {
        return Err(SignInError::Vendor(
            "the ID token isn't this sign-in's".to_string(),
        ));
    }
    if let Some(exp) = claims.get("exp").and_then(|e| e.as_f64()) {
        if (exp as i64) < unix_now() - 60 {
            return Err(SignInError::Vendor("the ID token has expired".to_string()));
        }
    }
    Ok(())
}

/// The plan, from the ID token or the access token (magpie `siwcPlan`).
fn plan_of(id_claims: &serde_json::Value, access: &str) -> String {
    let from = |claims: &serde_json::Value| {
        claims
            .get("https://api.openai.com/auth")
            .and_then(|auth| auth.get("chatgpt_plan_type"))
            .and_then(|p| p.as_str())
            .map(str::to_string)
            .unwrap_or_default()
    };
    let plan = from(id_claims);
    if !plan.is_empty() {
        return plan;
    }
    jwt_claims(access).map(|c| from(&c)).unwrap_or_default()
}

/// A value at a dotted-free path in the claims.
fn claim_string(claims: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let mut value = claims;
    for key in keys {
        value = value.get(*key)?;
    }
    value.as_str().map(str::to_string)
}

/// Decode a JWT's payload without checking it (magpie `jwtClaims`).
pub fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The pages a browser is shown once the callback is handled.
pub const SIWC_SUCCESS_PAGE: &str = SUCCESS_PAGE;
pub const SIWC_ERROR_PAGE: &str = ERROR_PAGE;
pub const SIWC_MISMATCH_PAGE: &str = MISMATCH_PAGE;
