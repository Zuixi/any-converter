//! First-class OAuth account resolution for upstream requests.
//!
//! A provider configured with `account = "<provider>:<id>"` signs its
//! upstream requests with that account's OAuth access token instead of
//! `api_key`. [`AccountResolver`] reads the credential from the
//! [`FileSecretStore`], refreshes it 5 minutes before expiry (mirroring
//! magpie's refresh margin), and single-flights concurrent refreshes so a
//! rotated refresh token is never spent twice.
//!
//! Model discovery ([`AccountResolver::project`] /
//! [`AccountResolver::list_models`]) asks Code Assist's `loadCodeAssist` the
//! way Gemini CLI does: it finds the Google Cloud project the account's
//! requests go to (onboarding the account the first time), reads the
//! experiment flags, and offers the models those experiments select from the
//! static Gemini table. The project and flags are cached per account for an
//! hour; when Code Assist can't be asked, the full static table stands in
//! (magpie's fallback semantics).
//!
//! Refreshes are additionally serialized **across processes** through a
//! [`crate::lockfile::FileLock`] next to the secrets file (the CLI and the
//! server are separate processes and must never spend the same refresh
//! token concurrently — vendors answer with `refresh_token_reused` and
//! revoke the sign-in). Accounts flagged revoked by an upstream
//! `token_revoked` / `invalid_grant` / `refresh_token_reused` are marked
//! in `revoked.json` next to the secrets file and refuse to resolve until
//! [`AccountResolver::clear_revoked`] runs after a fresh sign-in.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use thiserror::Error;

use super::Credential;
use super::codex_store::{TokenUpdate, write_tokens};
use super::gemini::GeminiEndpoints;
use super::gemini_models::{self, GeminiFlags};
use super::store::{self, FileSecretStore, SecretStore, SecretStoreError};
use crate::lockfile::FileLock;

/// A usable account: the id plus a ready-to-send access token.
#[derive(Debug, Clone)]
pub struct ResolvedAccount {
    pub id: String,
    pub access_token: String,
}

/// The Code Assist project the account's requests go to, and the plan the
/// account is on (magpie `googleProject`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAssistProject {
    pub id: String,
    pub plan: String,
}

/// Failures of account resolution / refresh.
#[derive(Debug, Error)]
pub enum AccountError {
    /// No credential is stored under the requested account id.
    #[error("account {0} is not signed in")]
    NotSignedIn(String),
    /// The stored credential exists but is not an OAuth credential.
    #[error("account {0} is not an OAuth credential")]
    NotOAuth(String),
    /// The account's provider has no supported token endpoint.
    #[error("provider of account {0} is not supported for OAuth signing")]
    UnsupportedProvider(String),
    /// The token endpoint rejected the refresh or the response was unusable.
    #[error("token refresh failed: {0}")]
    RefreshFailed(String),
    /// The account was flagged revoked after an upstream `token_revoked` /
    /// `invalid_grant` / `refresh_token_reused`; sign in again.
    #[error("account {0} is revoked ({1}); please sign in again (`any-converter accounts add`)")]
    Revoked(String, String),
    /// Code Assist's project/model endpoints failed (HTTP or parse).
    #[error("code assist: {0}")]
    CodeAssist(String),
    /// The account's sign-in is served to Code Assist Standard/Enterprise,
    /// which bill a Google Cloud project the user must name.
    #[error("{0}")]
    NeedsProject(String),
    /// The cross-process refresh lock could not be acquired.
    #[error("refresh lock error: {0}")]
    Lock(#[from] crate::lockfile::LockError),
    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Secret-store read/write failure.
    #[error("secret store error: {0}")]
    Store(#[from] SecretStoreError),
}

/// The Antigravity build this setup names (magpie `antigravityVersion`,
/// which reads the installed app and falls back to this).
pub const ANTIGRAVITY_VERSION: &str = "1.0.0";

/// Whether this account is an Antigravity sign-in.
pub fn is_antigravity(account_id: &str) -> bool {
    account_id.split(':').next() == Some("antigravity")
}

/// How early before `expires_at_unix` a token counts as expired.
pub const REFRESH_MARGIN_SECS: i64 = 300;

/// How long a found Code Assist project (and its experiment flags) stays
/// cached per account (magpie caches the flags for an hour).
pub const PROJECT_CACHE_SECS: i64 = 3600;

/// How long onboarding waits between `onboardUser` asks (magpie
/// `onboardPoll`).
const ONBOARD_POLL: Duration = Duration::from_secs(2);

/// ChatGPT's OAuth token endpoint, where Codex CLI itself renews its
/// sign-in (magpie `codexTokenURL`).
pub const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// Codex CLI's own public OAuth client id (magpie `codexClientID`; a
/// public-client identifier, not a secret).
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// The scope the codex refresh grant asks for (magpie `codexRefresh`).
const CODEX_REFRESH_SCOPE: &str = "openid profile email";

/// How long [`AccountResolver::resolve`] waits for the cross-process
/// refresh lock before failing the request.
const LOCK_TIMEOUT: Duration = crate::lockfile::DEFAULT_ACQUIRE_TIMEOUT;

/// One entry of `revoked.json`: why the account was flagged and when.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RevokedEntry {
    why: String,
    at: i64,
}

/// In-memory view of `revoked.json`. Reloaded whenever the file's mtime
/// changes, because the CLI and the server are different processes.
#[derive(Debug, Default)]
struct RevokedCache {
    entries: HashMap<String, RevokedEntry>,
    mtime: Option<SystemTime>,
}

/// The OAuth credential fields needed by [`AccountResolver`].
struct StoredOAuth {
    provider: String,
    account: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at_unix: Option<i64>,
}

/// Resolves account ids to OAuth access tokens, refreshing as needed.
///
/// Cheap to share: all state lives behind interior mutability, so the router
/// hands out one [`Arc`] per process.
pub struct AccountResolver {
    store: Arc<FileSecretStore>,
    client: reqwest::Client,
    /// Token endpoint refreshes are sent to (defaults to the public Gemini
    /// CLI client endpoint).
    token_url: String,
    /// ChatGPT's token endpoint (codex account refreshes).
    codex_token_url: String,
    /// In-memory cache: account id → (access_token, expires_at_unix).
    cache: Mutex<HashMap<String, (String, Option<i64>)>>,
    /// Per-account single-flight refresh locks.
    flight_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// How long to wait for the cross-process refresh lock.
    lock_timeout: Duration,
    /// Cached `revoked.json` state (see [`RevokedCache`]).
    revoked: Mutex<RevokedCache>,
    /// In-memory project cache: account id → (project, flags, at).
    projects: Mutex<HashMap<String, (CodeAssistProject, GeminiFlags, i64)>>,
    /// In-memory quota window cache: account id → (read_at, windows).
    /// Written by `credentials::allowance` (P9).
    allowances: Mutex<HashMap<String, (i64, super::allowance::Allowance)>>,
    /// Accounts whose allowance is being read right now, so a burst of
    /// requests starts one background read rather than one each.
    pub(crate) reading: Mutex<std::collections::HashSet<String>>,
    /// Where a Sign in with ChatGPT account's client id is kept
    /// (`siwc-accounts.json`); injectable so tests never touch the user's.
    siwc_meta_path: std::path::PathBuf,
}

impl AccountResolver {
    /// Create a resolver over `store` using the default token endpoints
    /// (Gemini's for gemini accounts, ChatGPT's for codex accounts).
    pub fn new(store: FileSecretStore, client: reqwest::Client) -> Self {
        Self::with_token_urls(
            store,
            client,
            GeminiEndpoints::default().token_url,
            CODEX_TOKEN_URL.to_string(),
        )
    }

    /// Create a resolver that refreshes against a specific `token_url` for
    /// every provider (test seam; production uses [`Self::new`], which
    /// points codex accounts at ChatGPT's real token endpoint).
    /// Where a Sign in with ChatGPT client id saved by an older version is
    /// still read from (`siwc-accounts.json`). New sign-ins keep it in the
    /// account's own entry, so this is only a fallback.
    pub fn with_siwc_meta_path(mut self, path: std::path::PathBuf) -> Self {
        self.siwc_meta_path = path;
        self
    }

    /// A ChatGPT API account's extra fields: the account's own entry first,
    /// the old side file second.
    fn siwc_meta_of(&self, account_id: &str) -> Option<super::SiwcMeta> {
        if let Ok(Some(Credential::OAuth {
            siwc: Some(meta), ..
        })) = self.store.get(account_id)
        {
            return Some(meta);
        }
        super::siwc_store::get(&self.siwc_meta_path, account_id)
    }

    pub fn with_token_url(
        store: FileSecretStore,
        client: reqwest::Client,
        token_url: String,
    ) -> Self {
        Self::with_token_urls(store, client, token_url.clone(), token_url)
    }

    fn with_token_urls(
        store: FileSecretStore,
        client: reqwest::Client,
        token_url: String,
        codex_token_url: String,
    ) -> Self {
        Self {
            store: Arc::new(store),
            client,
            token_url,
            codex_token_url,
            cache: Mutex::new(HashMap::new()),
            flight_locks: Mutex::new(HashMap::new()),
            lock_timeout: LOCK_TIMEOUT,
            revoked: Mutex::new(RevokedCache::default()),
            projects: Mutex::new(HashMap::new()),
            allowances: Mutex::new(HashMap::new()),
            reading: Mutex::new(std::collections::HashSet::new()),
            siwc_meta_path: super::siwc_store::default_path(),
        }
    }

    /// Override the cross-process refresh lock timeout (test seam; the
    /// production default is [`crate::lockfile::DEFAULT_ACQUIRE_TIMEOUT`]).
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    /// Create a resolver over the default secrets path
    /// ([`crate::config::secrets_path`]).
    pub fn with_default_store(client: reqwest::Client) -> Arc<Self> {
        Arc::new(Self::new(
            FileSecretStore::new(crate::config::secrets_path()),
            client,
        ))
    }

    /// Resolve `account_id` (e.g. `gemini:me@example.com`) to a ready-to-use
    /// access token, refreshing the credential when it is within
    /// [`REFRESH_MARGIN_SECS`] of expiry.
    pub async fn resolve(&self, account_id: &str) -> Result<ResolvedAccount, AccountError> {
        // Revoked accounts never resolve until signed in again.
        if let Some(entry) = self.revoked_entry(account_id) {
            return Err(AccountError::Revoked(account_id.to_string(), entry.why));
        }

        // Fast path: a cached token that is still valid skips the store.
        if let Some((token, expires_at_unix)) = self.cache_get(account_id) {
            if !expired(expires_at_unix) {
                return Ok(ResolvedAccount {
                    id: account_id.to_string(),
                    access_token: token,
                });
            }
        }

        let stored = self.read_oauth(account_id)?;
        if !expired(stored.expires_at_unix) {
            self.cache_put(
                account_id,
                stored.access_token.clone(),
                stored.expires_at_unix,
            );
            return Ok(ResolvedAccount {
                id: account_id.to_string(),
                access_token: stored.access_token,
            });
        }

        // Refresh under the per-account single-flight lock.
        let lock = self.flight_lock(account_id);
        let _guard = lock.lock().await;

        // Re-check under the lock: a concurrent winner may have refreshed
        // already. A changed refresh token means the credential was rotated
        // by someone else; an unexpired one means a winner refreshed without
        // rotation (Google does not rotate refresh tokens). Either way the
        // freshly persisted credential is authoritative.
        let seen_refresh_token = stored.refresh_token;
        let latest = self.read_oauth(account_id)?;
        if latest.refresh_token != seen_refresh_token || !expired(latest.expires_at_unix) {
            self.cache_put(
                account_id,
                latest.access_token.clone(),
                latest.expires_at_unix,
            );
            return Ok(ResolvedAccount {
                id: account_id.to_string(),
                access_token: latest.access_token,
            });
        }

        // Cross-process re-check: the CLI (or another server) may have
        // refreshed and persisted while we waited for the file lock. Key
        // the decision on the refresh token actually spent, mirroring
        // magpie's per-token `logins.json.codex.lock`.
        let _file_guard = self.refresh_lock().acquire(self.lock_timeout).await?;
        let latest = self.read_oauth(account_id)?;
        if latest.refresh_token != seen_refresh_token || !expired(latest.expires_at_unix) {
            self.cache_put(
                account_id,
                latest.access_token.clone(),
                latest.expires_at_unix,
            );
            return Ok(ResolvedAccount {
                id: account_id.to_string(),
                access_token: latest.access_token,
            });
        }

        self.refresh_stored(account_id, &latest).await
    }

    /// Refresh the account's token immediately, ignoring the expiry margin.
    ///
    /// Used after an upstream `token_expired` verdict: the locally cached
    /// expiry can still look valid while the vendor has rejected the
    /// access token. Serialized by both the per-account single-flight lock
    /// and the cross-process file lock.
    pub async fn force_refresh(&self, account_id: &str) -> Result<ResolvedAccount, AccountError> {
        if let Some(entry) = self.revoked_entry(account_id) {
            return Err(AccountError::Revoked(account_id.to_string(), entry.why));
        }

        let lock = self.flight_lock(account_id);
        let _guard = lock.lock().await;

        let baseline = self.read_oauth(account_id)?;
        let _file_guard = self.refresh_lock().acquire(self.lock_timeout).await?;
        let latest = self.read_oauth(account_id)?;
        if latest.refresh_token != baseline.refresh_token {
            // A concurrent process refreshed and rotated the refresh token
            // while we waited: spending the stale token would raise
            // `refresh_token_reused`. Reuse the winner's result instead.
            self.cache_put(
                account_id,
                latest.access_token.clone(),
                latest.expires_at_unix,
            );
            return Ok(ResolvedAccount {
                id: account_id.to_string(),
                access_token: latest.access_token,
            });
        }
        self.refresh_stored(account_id, &latest).await
    }

    /// Call the token endpoint for `latest` and persist the result.
    ///
    /// Callers must hold the per-account single-flight lock AND the
    /// cross-process file lock.
    async fn refresh_stored(
        &self,
        account_id: &str,
        latest: &StoredOAuth,
    ) -> Result<ResolvedAccount, AccountError> {
        let provider = account_id.split(':').next().unwrap_or("");
        // Sign in with ChatGPT has its own endpoint shape, its own refusal
        // codes and its own side file, so it renews on its own path — still
        // under the cross-process lock the caller holds (P9): its refresh
        // token rotates the same way.
        if provider == "chatgpt-api" {
            return self.refresh_siwc(account_id, latest).await;
        }
        let token_url = match provider {
            "gemini" => self.token_url.as_str(),
            "codex" => self.codex_token_url.as_str(),
            _ => return Err(AccountError::UnsupportedProvider(account_id.to_string())),
        };
        let refresh_token = latest.refresh_token.clone().ok_or_else(|| {
            AccountError::RefreshFailed("token expired and no refresh token is stored".to_string())
        })?;

        // Gemini's refresh is the classic form grant; Codex CLI's own
        // refresh is a JSON grant (magpie `codexRefresh`: Codex's public
        // client id, the refresh token, and a fixed scope).
        let (content_type, body) = if provider == "codex" {
            (
                "application/json",
                serde_json::json!({
                    "client_id": CODEX_CLIENT_ID,
                    "grant_type": "refresh_token",
                    "refresh_token": refresh_token,
                    "scope": CODEX_REFRESH_SCOPE,
                })
                .to_string(),
            )
        } else {
            let endpoints = GeminiEndpoints::default();
            (
                "application/x-www-form-urlencoded",
                form_urlencoded(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &refresh_token),
                    ("client_id", &endpoints.client_id),
                    ("client_secret", &endpoints.client_secret),
                ]),
            )
        };

        let response = self
            .client
            .post(token_url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .map_err(|e| {
                AccountError::RefreshFailed(format!("token endpoint request failed: {e}"))
            })?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| AccountError::RefreshFailed(format!("token endpoint read failed: {e}")))?;
        if !status.is_success() {
            return Err(AccountError::RefreshFailed(format!(
                "token endpoint returned {status}: {text}"
            )));
        }

        #[derive(serde::Deserialize)]
        struct RefreshResponse {
            access_token: String,
            #[serde(default)]
            refresh_token: Option<String>,
            #[serde(default)]
            expires_in: Option<u64>,
            #[serde(default)]
            id_token: Option<String>,
        }
        let parsed: RefreshResponse = serde_json::from_str(&text).map_err(|e| {
            AccountError::RefreshFailed(format!("token endpoint returned malformed payload: {e}"))
        })?;

        let expires_at_unix = match parsed.expires_in {
            Some(secs) => Some(unix_now() + secs as i64),
            // ChatGPT's token endpoint answers no `expires_in`: the new
            // access token's own `exp` claim is the expiry (magpie
            // `codexRefresh`).
            None if provider == "codex" => access_token_exp(&parsed.access_token),
            None => None,
        };
        // Google does not rotate refresh tokens; keep the old one when
        // absent. ChatGPT rotates them, and a response that omits one
        // keeps the stored one too.
        let new_refresh_token = parsed
            .refresh_token
            .or_else(|| latest.refresh_token.clone());

        // What an adopted codex account's auth.json receives: the rotated
        // tokens, written back under the same cross-process refresh lock
        // this refresh holds (magpie's `logins.json.codex.lock` spans the
        // refresh and the write-back). The id_token is only rewritten when
        // the endpoint rotated one.
        let write_back = TokenUpdate {
            access_token: parsed.access_token.clone(),
            refresh_token: new_refresh_token.clone().unwrap_or_default(),
            id_token: parsed.id_token.clone(),
        };

        let updated = Credential::OAuth {
            provider: latest.provider.clone(),
            account: latest.account.clone(),
            access_token: parsed.access_token.clone(),
            refresh_token: new_refresh_token,
            expires_at_unix,
            siwc: None,
        };
        self.store.put(account_id, &updated)?;
        self.cache_put(account_id, parsed.access_token.clone(), expires_at_unix);

        // Adopted codex accounts write back where Codex reads them; the
        // disk work runs off the async workers, and a failed write-back
        // never fails the request — the new tokens are already in the
        // secret store, and the next refresh retries the write-back
        // (magpie's write-back is best-effort for the same reason).
        if let Some(home) = self.adopted_home_of(account_id) {
            let outcome =
                tokio::task::spawn_blocking(move || write_tokens(&home, &write_back)).await;
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    log::warn!("codex write-back failed for account {account_id}: {error}");
                }
                Err(error) => {
                    log::warn!("codex write-back task failed for account {account_id}: {error}");
                }
            }
        }

        Ok(ResolvedAccount {
            id: account_id.to_string(),
            access_token: parsed.access_token,
        })
    }

    /// Renew a Sign in with ChatGPT account (P16). The client id OpenAI
    /// issued at sign-in comes from the side file; a refresh token OpenAI
    /// will never take again marks the account revoked, exactly as a
    /// `token_revoked` from the API would.
    async fn refresh_siwc(
        &self,
        account_id: &str,
        latest: &StoredOAuth,
    ) -> Result<ResolvedAccount, AccountError> {
        // The client id rides in the account's own entry now: one ledger,
        // so it can't be lost while the tokens survive.
        let meta = self.siwc_meta_of(account_id).ok_or_else(|| {
            AccountError::RefreshFailed(format!(
                "no ChatGPT API client id is kept for {account_id}; sign in again"
            ))
        })?;
        let refresh_token = latest.refresh_token.clone().ok_or_else(|| {
            AccountError::RefreshFailed("token expired and no refresh token is stored".to_string())
        })?;
        let sign_in = super::siwc_signin::SiwcSignIn::with_http(
            super::siwc_signin::SiwcEndpoints::default(),
            std::sync::Arc::new(super::siwc_signin::ReqwestSiwc::new(self.client.clone())),
        )
        .with_meta_path(self.siwc_meta_path.clone());
        let request = super::siwc_signin::SiwcRefreshRequest {
            client_id: &meta.client_id,
            refresh_token: &refresh_token,
            access_token: &latest.access_token,
            id_token: &meta.id_token,
            scopes: &meta.scopes,
            expires_at_unix: latest.expires_at_unix.unwrap_or(0),
            force: false,
        };
        match sign_in.renew(request).await {
            Ok(Some(tokens)) => {
                let expires_at_unix = Some(unix_now() + tokens.expires_in);
                let updated = Credential::OAuth {
                    provider: latest.provider.clone(),
                    account: latest.account.clone(),
                    access_token: tokens.access.clone(),
                    refresh_token: Some(tokens.refresh.clone()),
                    expires_at_unix,
                    siwc: Some(super::SiwcMeta {
                        client_id: meta.client_id.clone(),
                        host_id: meta.host_id.clone(),
                        id_token: if tokens.id_token.is_empty() {
                            meta.id_token.clone()
                        } else {
                            tokens.id_token.clone()
                        },
                        scopes: tokens
                            .scope
                            .split_ascii_whitespace()
                            .map(str::to_string)
                            .collect(),
                    }),
                };
                self.store.put(account_id, &updated)?;
                self.cache_put(account_id, tokens.access.clone(), expires_at_unix);
                Ok(ResolvedAccount {
                    id: account_id.to_string(),
                    access_token: tokens.access,
                })
            }
            // The token in hand still does (a hiccup, not a refusal).
            Ok(None) => Ok(ResolvedAccount {
                id: account_id.to_string(),
                access_token: latest.access_token.clone(),
            }),
            Err(super::siwc_signin::SiwcError::SignInExpired) => {
                let why = format!("{account_id}'s ChatGPT sign-in has expired — sign in again");
                if let Err(error) = self.mark_revoked(account_id, &why).await {
                    log::warn!("could not mark {account_id} revoked: {error}");
                }
                Err(AccountError::Revoked(account_id.to_string(), why))
            }
            Err(err) => Err(AccountError::RefreshFailed(err.to_string())),
        }
    }

    /// The Code Assist project the account's requests go to, found the way
    /// Gemini CLI does it (magpie `googleAccount.project`). Cached per
    /// account for an hour.
    ///
    /// `project_override` is the GCP project the user named; it wins when
    /// Code Assist asks the user for one. It is meant to come from the
    /// provider's `project` config field, which is not wired up yet (out of
    /// scope for this change), so callers currently pass `None`.
    pub async fn project(
        &self,
        account_id: &str,
        base: &str,
        project_override: Option<&str>,
    ) -> Result<CodeAssistProject, AccountError> {
        Ok(self
            .project_and_flags(account_id, base, project_override)
            .await?
            .0)
    }

    /// The models Code Assist offers the account (magpie `models()`).
    ///
    /// The project and the experiment flags come from one `loadCodeAssist`
    /// reply and are cached together (see [`Self::project`]);
    /// [`gemini_models::offered`] then applies Gemini CLI's skip table.
    /// Preview models need quota evidence (`listModels`), which this
    /// project doesn't read, so `preview` stays `false` until a quota
    /// source exists. When Code Assist can't be asked — any failure — the
    /// full static table stands in (magpie's fallback semantics), with one
    /// warning.
    pub async fn list_models(
        &self,
        account_id: &str,
        base: &str,
    ) -> Result<Vec<(String, String)>, AccountError> {
        match self.discover_models(account_id, base).await {
            Ok(models) => Ok(models),
            Err(err) => {
                log::warn!(
                    "code assist model discovery failed for account {account_id}: {err}; \
                     falling back to the full static table"
                );
                Ok(gemini_models::GEMINI_MODELS
                    .iter()
                    .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
                    .collect())
            }
        }
    }

    /// The models Code Assist offers the account, without the static
    /// fallback [`Self::list_models`] keeps for the global model catalog.
    ///
    /// Callers that name one account must be able to say discovery failed;
    /// presenting the full Gemini table as that account's list is wrong and
    /// hides the sign-in/project problem behind plausible-looking models.
    pub async fn discover_models(
        &self,
        account_id: &str,
        base: &str,
    ) -> Result<Vec<(String, String)>, AccountError> {
        let (_, flags) = self.project_and_flags(account_id, base, None).await?;
        Ok(gemini_models::offered(flags, false))
    }

    /// [`Self::project`] plus the experiment flags from the same reply;
    /// both cached for an hour under the account id.
    async fn project_and_flags(
        &self,
        account_id: &str,
        base: &str,
        project_override: Option<&str>,
    ) -> Result<(CodeAssistProject, GeminiFlags), AccountError> {
        if let Some((project, flags, at)) = self.projects_get(account_id) {
            if unix_now() - at < PROJECT_CACHE_SECS {
                return Ok((project, flags));
            }
        }

        let account = self.resolve(account_id).await?;
        let base = base.trim_end_matches('/');

        // Antigravity is set up as Antigravity, not as Gemini CLI: Google
        // serves it a different project and tier for `ideType: ANTIGRAVITY`
        // (magpie `antigravityProject`, google.go:726). Everything else
        // asks as the Gemini CLI does.
        let antigravity = is_antigravity(account_id);
        let mut metadata = if antigravity {
            serde_json::json!({ "ideType": "ANTIGRAVITY" })
        } else {
            serde_json::json!({
                "ideType": "IDE_UNSPECIFIED",
                "platform": "PLATFORM_UNSPECIFIED",
                "pluginType": "GEMINI",
            })
        };
        let mut body = serde_json::json!({});
        if let Some(want) = project_override {
            metadata["duetProject"] = serde_json::json!(want);
            body["cloudaicompanionProject"] = serde_json::json!(want);
        }
        body["metadata"] = metadata.clone();

        let url = format!("{base}/v1internal:loadCodeAssist");
        let reply = self
            .code_assist_post(&url, &account.access_token, &body)
            .await?;

        let flags = gemini_models::flags_from_experiments(&reply);
        let project = companion_project(reply.get("cloudaicompanionProject"));
        let plan = plan_of(&reply);

        // The account is already set up: its project rides in the reply,
        // or the one the user named stands in when Code Assist asks for
        // one.
        if reply
            .get("currentTier")
            .is_some_and(serde_json::Value::is_object)
        {
            if !project.is_empty() {
                let found = CodeAssistProject { id: project, plan };
                self.projects_put(account_id, found.clone(), flags);
                return Ok((found, flags));
            }
            if let Some(want) = project_override {
                let found = CodeAssistProject {
                    id: want.to_string(),
                    plan,
                };
                self.projects_put(account_id, found.clone(), flags);
                return Ok((found, flags));
            }
            return Err(needs_project(account_id, ""));
        }

        // No tier: take the default one and onboard the account, unless
        // that tier bills a project the user must name first.
        let tier = default_tier(&reply);
        if tier.user_defined && project_override.is_none() {
            return Err(needs_project(account_id, &ineligible_reason(&reply)));
        }

        let req = if antigravity {
            // Antigravity names the tier and itself in snake_case, and
            // onboards against the host its requests use (google.go:747).
            let mut tier_id = tier.id.clone();
            if tier_id.is_empty() || tier_id == "legacy-tier" {
                tier_id = "free-tier".to_string();
            }
            serde_json::json!({
                "tier_id": tier_id,
                "metadata": {
                    "ide_type": "ANTIGRAVITY",
                    "ide_name": "antigravity",
                    "ide_version": ANTIGRAVITY_VERSION,
                },
            })
        } else {
            let mut req = serde_json::json!({ "tierId": tier.id, "metadata": metadata });
            if tier.id != "free-tier" {
                req["cloudaicompanionProject"] = serde_json::json!(project_override.unwrap_or(""));
            }
            req
        };
        let mut id = self.onboard(base, &account.access_token, &req).await?;
        if id.is_empty() {
            id = project_override.unwrap_or("").to_string();
        }
        if id.is_empty() {
            return Err(needs_project(account_id, ""));
        }
        let found = CodeAssistProject {
            id,
            plan: tier.name,
        };
        self.projects_put(account_id, found.clone(), flags);
        Ok((found, flags))
    }

    /// Ask Code Assist to set the account up (`onboardUser`), polling the
    /// long-running reply the way magpie does.
    async fn onboard(
        &self,
        base: &str,
        access_token: &str,
        req: &serde_json::Value,
    ) -> Result<String, AccountError> {
        let url = format!("{base}/v1internal:onboardUser");
        for _ in 0..5 {
            let reply = self.code_assist_post(&url, access_token, req).await?;
            let project = reply
                .get("response")
                .and_then(|r| r.get("cloudaicompanionProject"))
                .map(|p| companion_project(Some(p)))
                .unwrap_or_default();
            let done = reply
                .get("done")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if done || !project.is_empty() {
                return Ok(project);
            }
            tokio::time::sleep(ONBOARD_POLL).await;
        }
        Err(AccountError::CodeAssist(
            "setting the account up for Code Assist is taking long; try again in a minute"
                .to_string(),
        ))
    }

    /// POST one of Code Assist's methods, signed with the account's access
    /// token (magpie `googleAccount.call`).
    async fn code_assist_post(
        &self,
        url: &str,
        access_token: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, AccountError> {
        let response = self
            .client
            .post(url)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .await
            .map_err(|e| AccountError::CodeAssist(format!("{url} request failed: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| AccountError::CodeAssist(format!("{url} read failed: {e}")))?;
        if !status.is_success() {
            return Err(AccountError::CodeAssist(format!(
                "{url} returned {status}: {text}"
            )));
        }
        serde_json::from_str(&text)
            .map_err(|e| AccountError::CodeAssist(format!("{url} returned malformed payload: {e}")))
    }

    /// Mark `account_id` revoked with reason `why`, persisting the marker
    /// to `revoked.json` next to the secrets file (`0600`, atomic write).
    pub async fn mark_revoked(&self, account_id: &str, why: &str) -> Result<(), AccountError> {
        let mut cache = self.revoked.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = load_revoked_file(&self.revoked_path());
        entries.insert(
            account_id.to_string(),
            RevokedEntry {
                why: why.to_string(),
                at: unix_now(),
            },
        );
        let bytes = serde_json::to_vec_pretty(&entries).map_err(SecretStoreError::from)?;
        store::write_private(&self.revoked_path(), &bytes)?;
        cache.entries = entries;
        cache.mtime = revoked_mtime(&self.revoked_path());
        Ok(())
    }

    /// Remove the revoked marker for `account_id` (call after a fresh
    /// sign-in so the account can resolve again).
    pub async fn clear_revoked(&self, account_id: &str) -> Result<(), AccountError> {
        let mut cache = self.revoked.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = load_revoked_file(&self.revoked_path());
        if entries.remove(account_id).is_some() {
            let bytes = serde_json::to_vec_pretty(&entries).map_err(SecretStoreError::from)?;
            store::write_private(&self.revoked_path(), &bytes)?;
            cache.entries = entries;
            cache.mtime = revoked_mtime(&self.revoked_path());
        }
        // Drop any stale in-memory marker even when the file had none.
        cache.entries.remove(account_id);
        Ok(())
    }

    /// Whether `account_id` is currently flagged revoked.
    pub fn is_revoked(&self, account_id: &str) -> bool {
        self.revoked_entry(account_id).is_some()
    }

    /// Persist a fresh sign-in result and clear any revoked marker (P10:
    /// sign-ins now flow through the server's task API, not just the CLI).
    pub async fn store_sign_in(
        &self,
        account_id: &str,
        credential: Credential,
    ) -> Result<(), AccountError> {
        if let Credential::OAuth {
            access_token,
            expires_at_unix,
            ..
        } = &credential
        {
            self.cache_put(account_id, access_token.clone(), *expires_at_unix);
        }
        self.store.put(account_id, &credential)?;
        self.clear_revoked(account_id).await?;
        Ok(())
    }

    /// The stored credential for `account_id` (P10 sign-out reads it
    /// before deleting so the refresh token can be revoked).
    pub(crate) fn stored(&self, account_id: &str) -> Result<Option<Credential>, SecretStoreError> {
        self.store.get(account_id)
    }

    /// Every stored credential (P10 `GET /v1/accounts`; callers must never
    /// serialize the tokens).
    pub(crate) fn list_credentials(&self) -> Result<Vec<(String, Credential)>, SecretStoreError> {
        let mut out = Vec::new();
        for id in self.store.list()? {
            if let Some(credential) = self.store.get(&id)? {
                out.push((id, credential));
            }
        }
        Ok(out)
    }

    /// Remove the stored credential and its cached tokens (P10 sign-out;
    /// the revocation request happens first in `credentials::signout`).
    pub async fn delete_stored(&self, account_id: &str) -> Result<(), AccountError> {
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(account_id);
        self.store.delete(account_id)?;
        Ok(())
    }

    /// The revoked marker file: `revoked.json` next to the secrets file.
    fn revoked_path(&self) -> PathBuf {
        match self.store.path().parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join("revoked.json"),
            _ => PathBuf::from("revoked.json"),
        }
    }

    /// The cross-process refresh lock for this store (one per secrets file).
    fn refresh_lock(&self) -> FileLock {
        let mut path = self.store.path().as_os_str().to_owned();
        path.push(".refresh.lock");
        FileLock::new(PathBuf::from(path))
    }

    /// The CODEX_HOME an adopted account's refreshed tokens are written
    /// back to, when `account_id` was adopted (`accounts adopt`, P11b).
    fn adopted_home_of(&self, account_id: &str) -> Option<PathBuf> {
        load_adopted(&self.adopted_path())
            .get(account_id)
            .map(PathBuf::from)
    }

    /// The adoption registry: `adopted.json` next to the secrets file
    /// (same placement as `revoked.json`; the CLI and the server are
    /// different processes and must agree on the file).
    fn adopted_path(&self) -> PathBuf {
        adopted_path_of(self.store.path())
    }

    /// Current revoked entry for `account_id`, reloading `revoked.json`
    /// when its mtime changed since the last read.
    fn revoked_entry(&self, account_id: &str) -> Option<RevokedEntry> {
        let mut cache = self.revoked.lock().unwrap_or_else(|e| e.into_inner());
        let disk_mtime = revoked_mtime(&self.revoked_path());
        if cache.mtime != disk_mtime {
            cache.entries = load_revoked_file(&self.revoked_path());
            cache.mtime = disk_mtime;
        }
        cache.entries.get(account_id).cloned()
    }

    /// Read and validate the stored OAuth credential for `account_id`.
    fn read_oauth(&self, account_id: &str) -> Result<StoredOAuth, AccountError> {
        let credential = self
            .store
            .get(account_id)?
            .ok_or_else(|| AccountError::NotSignedIn(account_id.to_string()))?;
        match credential {
            Credential::OAuth {
                provider,
                account,
                access_token,
                refresh_token,
                expires_at_unix,
                ..
            } => Ok(StoredOAuth {
                provider,
                account,
                access_token,
                refresh_token,
                expires_at_unix,
            }),
            _ => Err(AccountError::NotOAuth(account_id.to_string())),
        }
    }

    fn cache_get(&self, account_id: &str) -> Option<(String, Option<i64>)> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account_id)
            .cloned()
    }

    fn cache_put(&self, account_id: &str, access_token: String, expires_at_unix: Option<i64>) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account_id.to_string(), (access_token, expires_at_unix));
    }

    fn projects_get(&self, account_id: &str) -> Option<(CodeAssistProject, GeminiFlags, i64)> {
        self.projects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account_id)
            .cloned()
    }

    fn projects_put(&self, account_id: &str, project: CodeAssistProject, flags: GeminiFlags) {
        self.projects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account_id.to_string(), (project, flags, unix_now()));
    }

    /// Get-or-create the per-account single-flight refresh lock.
    fn flight_lock(&self, account_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.flight_locks.lock().unwrap_or_else(|e| e.into_inner());
        locks
            .entry(account_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// The shared HTTP client (quota reads in `credentials::allowance`).
    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// The cached quota windows for `account_id`, with the unix time they
    /// were read at (P9).
    pub(crate) fn allowance_cache_get(
        &self,
        account_id: &str,
    ) -> Option<(i64, super::allowance::Allowance)> {
        self.allowances
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account_id)
            .cloned()
    }

    /// Cache `windows` for `account_id`, read now (P9).
    pub(crate) fn allowance_cache_put(
        &self,
        account_id: &str,
        windows: super::allowance::Allowance,
    ) {
        self.allowances
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account_id.to_string(), (unix_now(), windows));
    }

    /// The project id cached for `account_id`, when an earlier
    /// `loadCodeAssist` found one (quota asks ride it; no onboarding).
    pub(crate) fn cached_project_id(&self, account_id: &str) -> Option<String> {
        self.projects_get(account_id)
            .map(|(project, _, _)| project.id)
    }
}

/// Unknown expiry (`None`) counts as valid; a known expiry counts as expired
/// once it is within [`REFRESH_MARGIN_SECS`] of now.
fn expired(expires_at_unix: Option<i64>) -> bool {
    match expires_at_unix {
        None => false,
        Some(at) => unix_now() + REFRESH_MARGIN_SECS >= at,
    }
}

/// Code Assist's project arrives as a bare id string or as `{id, name}`
/// (magpie `companionProject`).
fn companion_project(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(id)) => id.clone(),
        Some(serde_json::Value::Object(obj)) => obj
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

/// One tier of a `loadCodeAssist` reply, as project discovery reads it.
struct CodeAssistTier {
    id: String,
    name: String,
    user_defined: bool,
}

fn tier_view(tier: &serde_json::Value) -> CodeAssistTier {
    CodeAssistTier {
        id: tier
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        name: tier
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        user_defined: tier
            .get("userDefinedCloudaicompanionProject")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

/// The tier to onboard to: the default one, the first, or a legacy tier
/// that always wants a user project (magpie `defaultTier`).
fn default_tier(reply: &serde_json::Value) -> CodeAssistTier {
    let tiers: &[serde_json::Value] = reply
        .get("allowedTiers")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for tier in tiers {
        if tier
            .get("isDefault")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return tier_view(tier);
        }
    }
    match tiers.first() {
        Some(tier) => tier_view(tier),
        None => CodeAssistTier {
            id: "legacy-tier".to_string(),
            name: String::new(),
            user_defined: true,
        },
    }
}

/// The plan the account is on: the paid tier's name, else the current
/// tier's name or id (magpie `plan`).
fn plan_of(reply: &serde_json::Value) -> String {
    let paid = reply.get("paidTier").filter(|tier| tier.is_object());
    if let Some(tier) = paid {
        if tier
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| !name.is_empty())
        {
            if let Some(plan) = tier_plan(tier) {
                return plan;
            }
        }
    }
    reply
        .get("currentTier")
        .filter(|tier| tier.is_object())
        .and_then(tier_plan)
        .unwrap_or_default()
}

/// A tier's name, or its id when unnamed.
fn tier_plan(tier: &serde_json::Value) -> Option<String> {
    let name = tier
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if !name.is_empty() {
        return Some(name.to_string());
    }
    let id = tier
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if !id.is_empty() {
        return Some(id.to_string());
    }
    None
}

/// Google's reason the account is ineligible, when it gave one (magpie
/// `needsProject`'s why).
fn ineligible_reason(reply: &serde_json::Value) -> String {
    reply
        .get("ineligibleTiers")
        .and_then(serde_json::Value::as_array)
        .map(|tiers| {
            tiers
                .iter()
                .filter_map(|tier| {
                    tier.get("reasonMessage")
                        .and_then(serde_json::Value::as_str)
                })
                .find(|message| !message.is_empty())
                .unwrap_or("")
        })
        .unwrap_or("")
        .to_string()
}

/// A Standard/Enterprise account needs a Google Cloud project named
/// (magpie `needsProject`).
fn needs_project(account_id: &str, why: &str) -> AccountError {
    let mut msg = format!(
        "Google serves this sign-in only to Gemini Code Assist Standard and Enterprise, \
         which bill a Google Cloud project: name one with the provider's `project` field \
         in config.toml for account {account_id}"
    );
    if !why.is_empty() {
        msg = format!("{why} — {msg}");
    }
    AccountError::NeedsProject(msg)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `application/x-www-form-urlencoded` body assembly (the workspace enables
/// no reqwest `form` feature, matching [`super::gemini`]'s hand-rolled encoder).
fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

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

/// Read `revoked.json` into a map; an unreadable or malformed file reads
/// as empty (a missing file is the normal state).
fn load_revoked_file(path: &std::path::Path) -> HashMap<String, RevokedEntry> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// `revoked.json`'s mtime, `None` when the file does not exist.
fn revoked_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// Clear the revoked marker for `account_id` on the default secrets store.
///
/// The CLI invokes this right after a successful sign-in (`accounts add`).
/// It performs no network I/O, so a throwaway client is enough.
pub async fn clear_revoked_default_store(account_id: &str) -> Result<(), AccountError> {
    AccountResolver::with_default_store(reqwest::Client::new())
        .clear_revoked(account_id)
        .await
}

/// Whether this account was adopted onto a Codex store, and where.
///
/// Adoption is kept in `adopted.json` beside the secrets file — the
/// credential itself is deliberately unchanged (importing agents in P11a
/// and every other construction site keeps working untouched):
/// `accounts adopt` records the account id against the CODEX_HOME whose
/// `auth.json` receives every refreshed token, and `accounts release`
/// removes the record. This reads the default secrets store's registry.
pub fn codex_home_of(cred: &Credential) -> Option<PathBuf> {
    let Credential::OAuth {
        provider, account, ..
    } = cred
    else {
        return None;
    };
    if provider != "codex" {
        return None;
    }
    load_adopted(&adopted_path_of(&crate::config::secrets_path()))
        .get(&format!("codex:{account}"))
        .map(PathBuf::from)
}

/// Mark `account_id` adopted onto the Codex store at `home`, in the
/// registry next to `secrets_path` (CLI `accounts adopt`, P11b).
pub fn mark_adopted(
    secrets_path: &std::path::Path,
    account_id: &str,
    home: &std::path::Path,
) -> Result<(), AccountError> {
    let path = adopted_path_of(secrets_path);
    let mut entries = load_adopted(&path);
    entries.insert(account_id.to_string(), home.to_string_lossy().into_owned());
    let bytes = serde_json::to_vec_pretty(&entries).map_err(SecretStoreError::from)?;
    store::write_private(&path, &bytes)?;
    Ok(())
}

/// Remove the adoption marker for `account_id` (CLI `accounts release`);
/// `Ok(false)` when there was none.
pub fn release_adopted(
    secrets_path: &std::path::Path,
    account_id: &str,
) -> Result<bool, AccountError> {
    let path = adopted_path_of(secrets_path);
    let mut entries = load_adopted(&path);
    if entries.remove(account_id).is_none() {
        return Ok(false);
    }
    let bytes = serde_json::to_vec_pretty(&entries).map_err(SecretStoreError::from)?;
    store::write_private(&path, &bytes)?;
    Ok(true)
}

/// `adopted.json` next to the secrets file: codex account id → the
/// CODEX_HOME its refreshed tokens are written back to.
fn adopted_path_of(secrets_path: &std::path::Path) -> PathBuf {
    match secrets_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("adopted.json"),
        _ => PathBuf::from("adopted.json"),
    }
}

/// Read `adopted.json` into a map; an unreadable or malformed file reads
/// as empty (a missing file is the normal state).
fn load_adopted(path: &std::path::Path) -> HashMap<String, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// The access token's own JWT `exp` claim in unix seconds, decoded without
/// checking the signature — ChatGPT's token endpoint answers no
/// `expires_in`, so the new access token itself carries the expiry
/// (magpie `codexRefresh` reads it the same way).
fn access_token_exp(access_token: &str) -> Option<i64> {
    let payload = access_token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("exp").and_then(serde_json::Value::as_i64)
}

/// base64url (RFC 4648 §5) decode, padding optional — hand-rolled because
/// the workspace adds no base64 dependency (the same helper
/// `credentials::agent_import` keeps for its own reads).
fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in input.trim_end_matches('=').bytes() {
        let value = match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a' + 26),
            b'0'..=b'9' => u32::from(c - b'0' + 52),
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_expired_boundary() {
        let far_future = Some(unix_now() + 3600);
        assert!(!expired(far_future));
        assert!(!expired(None), "unknown expiry is treated as valid");
        let within_margin = Some(unix_now() + REFRESH_MARGIN_SECS);
        assert!(expired(within_margin));
        let past = Some(unix_now() - 1);
        assert!(expired(past));
    }

    #[test]
    fn test_form_urlencoded() {
        let form = form_urlencoded(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "r/t+o&k=e n"),
        ]);
        assert_eq!(
            form,
            "grant_type=refresh_token&refresh_token=r%2Ft%2Bo%26k%3De%20n"
        );
    }

    #[test]
    fn test_access_token_exp_reads_the_jwt_exp_claim() {
        // header.payload.sig; payload {"exp": 1800000000}.
        let token = "eyJhbGciOiJub25lIn0.eyJleHAiOiAxODAwMDAwMDAwfQ.sig";
        assert_eq!(access_token_exp(token), Some(1_800_000_000));
        // A string `exp` is not a usable expiry.
        let wrong = "eyJhbGciOiJub25lIn0.eyJleHAiOiJ4In0.sig";
        assert_eq!(access_token_exp(wrong), None);
        assert_eq!(access_token_exp("not-a-jwt"), None);
        assert_eq!(access_token_exp("a.b"), None);
    }
    /// A Sign in with ChatGPT account renews through `AccountResolver` like
    /// every other provider — without it the account died an hour after
    /// signing in, with no path back but signing in again.
    #[tokio::test]
    async fn chatgpt_api_account_renews_through_the_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"));
        let id = "chatgpt-api:me@example.com";
        store
            .put(
                id,
                &Credential::OAuth {
                    provider: "chatgpt-api".into(),
                    account: "me@example.com".into(),
                    access_token: "old".into(),
                    refresh_token: Some("old-refresh".into()),
                    expires_at_unix: Some(unix_now() - 10),
                    siwc: None,
                },
            )
            .unwrap();
        // No side file: the client id the sign-in registered is missing, and
        // the resolver says so rather than sending a refresh that would be
        // refused.
        let resolver = AccountResolver::new(
            FileSecretStore::new(dir.path().join("secrets.json")),
            reqwest::Client::new(),
        )
        .with_siwc_meta_path(dir.path().join("siwc-accounts.json"));
        let err = resolver.resolve(id).await.unwrap_err();
        assert!(
            matches!(&err, AccountError::RefreshFailed(why) if why.contains("sign in again")),
            "{err}"
        );

        // The missing client id is an actionable error rather than a
        // refresh that OpenAI would refuse: the account is told to sign in
        // again, which is exactly what it needs.
        assert!(err.to_string().contains("ChatGPT API"), "{err}");
    }
}
