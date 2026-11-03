//! Sign-out revocation (magpie `forgetSIWCLogin`, chatgpt_api.go:131).
//!
//! Forgetting an account: read the stored token → ask the vendor to revoke
//! the refresh token when it publishes a revocation endpoint → remove the
//! account locally. A failed revocation never blocks the removal.
//!
//! The revocation endpoint is an optional capability published by the
//! vendor's metadata, not something this crate invents per provider:
//! Google publishes `https://oauth2.googleapis.com/revoke` for the Gemini
//! CLI OAuth client; Copilot/GitHub publish no revocation endpoint, so
//! those accounts are simply removed ([`RevokeOutcome::NotOffered`]).

use crate::config::build_proxied_client;
use crate::credentials::siwc_signin::SiwcSignIn;
use crate::credentials::siwc_store;
use crate::credentials::{AccountResolver, Credential};

/// How the revocation part of a removal went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// The vendor's endpoint accepted the revocation.
    Revoked,
    /// The vendor publishes no revocation endpoint: nothing was sent.
    NotOffered,
    /// A revocation endpoint exists but the call failed; the account was
    /// still removed locally (magpie `forgetSIWCLogin`).
    Failed(String),
}

/// Google's published token-revocation endpoint (Google docs,
/// `https://oauth2.googleapis.com/revoke`).
const GOOGLE_REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";

/// The revocation endpoint a provider's metadata publishes, when any.
/// Copilot/GitHub publish none → `None` (nothing is hardcoded for them).
fn revocation_endpoint(provider: &str) -> Option<&'static str> {
    match provider {
        "gemini" => Some(GOOGLE_REVOKE_URL),
        _ => None,
    }
}

/// Forget an account: ask the vendor to revoke the refresh token when it
/// publishes a revocation endpoint, then remove it locally. A failed
/// revocation never blocks the removal (magpie `forgetSIWCLogin`).
///
/// `proxy` is the account's egress proxy (provider.proxy > server.proxy),
/// used for the revocation request the same way the account's own
/// requests use it (P10).
pub async fn remove(
    accounts: &AccountResolver,
    id: &str,
    proxy: Option<&str>,
) -> Result<RevokeOutcome, String> {
    let provider = id.split(':').next().unwrap_or("");

    // Read the token first, revoke, then delete — in that order (magpie).
    let credential = accounts.stored(id).map_err(|error| error.to_string())?;

    // A Sign in with ChatGPT account revokes at the endpoint OpenAI
    // publishes in its OIDC metadata (magpie `siwcRevoke`), which is the
    // only place it is named.
    if provider == "chatgpt-api" {
        // Through the account's own exit, as every other request is: a
        // revocation that left from another address reads as a stranger.
        let client = build_proxied_client(proxy).unwrap_or_default();
        let refresh_token = credential.as_ref().and_then(|credential| match credential {
            Credential::OAuth { refresh_token, .. } => refresh_token.clone(),
            _ => None,
        });
        let outcome = match SiwcSignIn::new(client)
            .with_meta_path(siwc_store::default_path())
            .revoke(id, refresh_token.as_deref())
            .await
        {
            Ok(true) => RevokeOutcome::Revoked,
            Ok(false) => RevokeOutcome::NotOffered,
            Err(err) => RevokeOutcome::Failed(err.to_string()),
        };
        accounts
            .delete_stored(id)
            .await
            .map_err(|error| error.to_string())?;
        let _ = siwc_store::remove(&siwc_store::default_path(), id);
        return Ok(outcome);
    }

    let outcome = match revocation_endpoint(provider) {
        None => RevokeOutcome::NotOffered,
        Some(endpoint) => {
            let refresh_token = credential.as_ref().and_then(|credential| match credential {
                Credential::OAuth { refresh_token, .. } => refresh_token.clone(),
                _ => None,
            });
            match refresh_token {
                // Nothing to revoke (no refresh token was stored).
                None => RevokeOutcome::NotOffered,
                Some(token) => revoke(endpoint, &token, proxy).await,
            }
        }
    };

    // Local removal is unconditional: a failed revocation never blocks
    // forgetting the account.
    accounts
        .delete_stored(id)
        .await
        .map_err(|error| error.to_string())?;
    if let Err(err) = accounts.clear_revoked(id).await {
        log::warn!("failed to clear revoked marker for {id}: {err}");
    }
    Ok(outcome)
}

/// One revocation request: `POST {endpoint}?token={refresh_token}`
/// (Google's documented shape), through the account's proxy.
async fn revoke(endpoint: &str, refresh_token: &str, proxy: Option<&str>) -> RevokeOutcome {
    let client = match build_proxied_client(proxy) {
        Ok(client) => client,
        Err(error) => return RevokeOutcome::Failed(error.to_string()),
    };
    let url = format!("{endpoint}?token={}", percent_encode(refresh_token));
    match client.post(url).send().await {
        Ok(response) if response.status().is_success() => RevokeOutcome::Revoked,
        Ok(response) => RevokeOutcome::Failed(format!(
            "revocation endpoint returned {}",
            response.status()
        )),
        Err(error) => RevokeOutcome::Failed(format!("revocation request failed: {error}")),
    }
}

/// Percent-encode for query strings (unreserved set kept literal; the
/// workspace enables no urlencoding crate).
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
    use crate::credentials::{AccountResolver, FileSecretStore};

    fn resolver(dir: &tempfile::TempDir) -> AccountResolver {
        AccountResolver::new(
            FileSecretStore::new(dir.path().join("secrets.json")),
            reqwest::Client::new(),
        )
    }

    fn oauth(provider: &str, refresh_token: Option<&str>) -> Credential {
        Credential::OAuth {
            provider: provider.to_string(),
            account: "someone".to_string(),
            access_token: "ya29.access".to_string(),
            refresh_token: refresh_token.map(String::from),
            expires_at_unix: Some(1_900_000_000),
        }
    }

    #[tokio::test]
    async fn copilot_removes_without_revocation_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = resolver(&dir);
        accounts
            .store_sign_in("copilot:wdjb****", oauth("copilot", None))
            .await
            .unwrap();

        let outcome = remove(&accounts, "copilot:wdjb****", None).await.unwrap();
        assert_eq!(outcome, RevokeOutcome::NotOffered);
        assert_eq!(accounts.stored("copilot:wdjb****").unwrap(), None);
    }

    #[tokio::test]
    async fn gemini_without_refresh_token_is_not_offered_but_removed() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = resolver(&dir);
        accounts
            .store_sign_in("gemini:me@example.com", oauth("gemini", None))
            .await
            .unwrap();

        let outcome = remove(&accounts, "gemini:me@example.com", None)
            .await
            .unwrap();
        assert_eq!(outcome, RevokeOutcome::NotOffered);
        assert_eq!(accounts.stored("gemini:me@example.com").unwrap(), None);
    }

    #[tokio::test]
    async fn gemini_revocation_failure_still_removes_locally() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = resolver(&dir);
        accounts
            .store_sign_in("gemini:me@example.com", oauth("gemini", Some("1//refresh")))
            .await
            .unwrap();

        // Point the revocation at an unreachable endpoint via the proxy
        // seam: a proxy URL that refuses connections. The request fails,
        // the account is still removed, and the outcome says Failed.
        let outcome = remove(
            &accounts,
            "gemini:me@example.com",
            Some("http://127.0.0.1:1"),
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, RevokeOutcome::Failed(_)),
            "unexpected outcome: {outcome:?}"
        );
        assert_eq!(accounts.stored("gemini:me@example.com").unwrap(), None);
    }

    #[tokio::test]
    async fn removing_an_unknown_account_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = resolver(&dir);

        let outcome = remove(&accounts, "gemini:nobody@example.com", None)
            .await
            .unwrap();
        assert_eq!(outcome, RevokeOutcome::NotOffered);
    }

    #[test]
    fn percent_encode_query_safe() {
        assert_eq!(percent_encode("1//abc"), "1%2F%2Fabc");
        assert_eq!(percent_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(percent_encode("~-.Az09"), "~-.Az09");
    }
}
