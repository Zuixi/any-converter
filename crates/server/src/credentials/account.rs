//! First-class OAuth account resolution for upstream requests.
//!
//! A provider configured with `account = "<provider>:<id>"` signs its
//! upstream requests with that account's OAuth access token instead of
//! `api_key`. [`AccountResolver`] reads the credential from the
//! [`FileSecretStore`], refreshes it 5 minutes before expiry (mirroring
//! magpie's refresh margin), and single-flights concurrent refreshes so a
//! rotated refresh token is never spent twice.
//!
//! Model discovery (`loadCodeAssist`) deliberately lives in a later task:
//! the model inventory comes from config / `model_map`, not from the account.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use thiserror::Error;

use super::Credential;
use super::gemini::GeminiEndpoints;
use super::store::{FileSecretStore, SecretStore, SecretStoreError};

/// A usable account: the id plus a ready-to-send access token.
#[derive(Debug, Clone)]
pub struct ResolvedAccount {
    pub id: String,
    pub access_token: String,
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
    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Secret-store read/write failure.
    #[error("secret store error: {0}")]
    Store(#[from] SecretStoreError),
}

/// How early before `expires_at_unix` a token counts as expired.
pub const REFRESH_MARGIN_SECS: i64 = 300;

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
    /// In-memory cache: account id → (access_token, expires_at_unix).
    cache: Mutex<HashMap<String, (String, Option<i64>)>>,
    /// Per-account single-flight refresh locks.
    flight_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl AccountResolver {
    /// Create a resolver over `store` using the default Gemini token endpoint.
    pub fn new(store: FileSecretStore, client: reqwest::Client) -> Self {
        Self::with_token_url(store, client, GeminiEndpoints::default().token_url)
    }

    /// Create a resolver that refreshes against a specific `token_url` (test
    /// seam; production uses [`Self::new`]).
    pub fn with_token_url(
        store: FileSecretStore,
        client: reqwest::Client,
        token_url: String,
    ) -> Self {
        Self {
            store: Arc::new(store),
            client,
            token_url,
            cache: Mutex::new(HashMap::new()),
            flight_locks: Mutex::new(HashMap::new()),
        }
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

        let provider = account_id.split(':').next().unwrap_or("");
        if provider != "gemini" {
            return Err(AccountError::UnsupportedProvider(account_id.to_string()));
        }
        let refresh_token = latest.refresh_token.clone().ok_or_else(|| {
            AccountError::RefreshFailed("token expired and no refresh token is stored".to_string())
        })?;

        let endpoints = GeminiEndpoints::default();
        let form = form_urlencoded(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
            ("client_id", &endpoints.client_id),
            ("client_secret", &endpoints.client_secret),
        ]);

        let response = self
            .client
            .post(&self.token_url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form)
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
        }
        let parsed: RefreshResponse = serde_json::from_str(&text).map_err(|e| {
            AccountError::RefreshFailed(format!("token endpoint returned malformed payload: {e}"))
        })?;

        let expires_at_unix = parsed.expires_in.map(|secs| unix_now() + secs as i64);
        // Google does not rotate refresh tokens; keep the old one when absent.
        let new_refresh_token = parsed.refresh_token.or(latest.refresh_token);

        let updated = Credential::OAuth {
            provider: latest.provider,
            account: latest.account,
            access_token: parsed.access_token.clone(),
            refresh_token: new_refresh_token,
            expires_at_unix,
        };
        self.store.put(account_id, &updated)?;
        self.cache_put(account_id, parsed.access_token.clone(), expires_at_unix);

        Ok(ResolvedAccount {
            id: account_id.to_string(),
            access_token: parsed.access_token,
        })
    }

    /// Code Assist model discovery is not part of this task: the model list
    /// is configured via `config.toml` / `model_map`, not fetched from the
    /// account. Always returns an empty list.
    pub async fn load_models(
        &self,
        _account_id: &str,
        _base: &str,
    ) -> Result<Vec<String>, AccountError> {
        Ok(Vec::new())
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

    /// Get-or-create the per-account single-flight refresh lock.
    fn flight_lock(&self, account_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.flight_locks.lock().unwrap_or_else(|e| e.into_inner());
        locks
            .entry(account_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
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
}
