//! Token lifecycle management for OAuth credentials.
//!
//! Pure-logic layer: no network access here. The refresh function is injected
//! by the caller (T4/T5 will wire real endpoints).
//!
//! Lessons baked in from magpie:
//! - Refresh 5 minutes early (token may be revoked server-side slightly
//!   before local expiry).
//! - Single-flight refresh: with refresh-token rotation, concurrent refreshes
//!   cause `refresh_token_reused` and the account gets kicked. A
//!   [`tokio::sync::Mutex`] plus a re-check after acquiring the lock ensures
//!   concurrent waiters reuse the winner's result instead of refreshing again.
//! - 401 classification: only specific markers mean "revoked"; a plain
//!   "expired" must not revoke the credential.

use std::collections::HashMap;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;

/// The result of one successful refresh, keyed by the refresh token it
/// spent. Concurrent callers holding the same stale tokens (same refresh
/// token) reuse the winner's result; a caller holding a different (already
/// rotated) refresh token never gets a stale cached result.
struct RefreshCache {
    /// refresh token spent → tokens it produced, with the instant they were
    /// refreshed at (entries are kept for a bounded window).
    entries: HashMap<String, (OAuthTokens, Instant)>,
}

impl RefreshCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn get(&self, refresh_token: &str) -> Option<&OAuthTokens> {
        self.entries.get(refresh_token).map(|(t, _)| t)
    }

    fn put(&mut self, refresh_token: String, tokens: OAuthTokens) {
        // Bound the cache: keep only recent entries (one hour is far beyond
        // any plausible concurrent-refresh window).
        let cutoff = Instant::now() - Duration::from_secs(3600);
        self.entries.retain(|_, (_, at)| *at > cutoff);
        self.entries.insert(refresh_token, (tokens, Instant::now()));
    }
}

/// OAuth token pair plus local expiry.
///
/// Deliberately NOT `Serialize`: `Instant` has no portable serde
/// representation. The storage representation will be defined in T4 when real
/// endpoints are wired.
#[derive(Clone, Debug, PartialEq)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// `None` means the token never expires (or expiry unknown → treated as
    /// valid).
    pub expires_at: Option<Instant>,
}

/// Outcome of a [`TokenManager::current`] call regarding refresh behavior.
///
/// Kept as an explicit enum for T4/T5 callers that need to know whether to
/// persist new tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// A refresh happened; returned tokens are new and should be persisted.
    Refreshed,
    /// Cached tokens were still valid; returned tokens are unchanged.
    Unchanged,
}

/// 401 semantics classification (magpie `signin_revoked.go` /
/// `codex_expired.go`).
///
/// Only `token_revoked` / `invalid_grant` / `refresh_token_reused` mean the
/// credential is revoked. `token_expired` just means expired. Anything else is
/// [`AuthErr401::Unknown`] and must NOT mark the credential revoked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthErr401 {
    /// Token merely expired; a refresh may fix it.
    Expired,
    /// Credential revoked server-side; stop using it.
    Revoked,
    /// Unknown 401 semantics; do not mark anything.
    Unknown,
}

/// Classify a 401 response body (case-insensitive substring match).
///
/// - `token_expired` → [`AuthErr401::Expired`]
/// - `token_revoked` | `invalid_grant` | `refresh_token_reused` →
///   [`AuthErr401::Revoked`]
/// - anything else (including empty body) → [`AuthErr401::Unknown`]
pub fn classify_401(body: &str) -> AuthErr401 {
    // Marker precedence: revoked markers win over expired markers if both
    // appear (a revoked token is the stronger signal).
    let lower = body.to_ascii_lowercase();
    if lower.contains("token_revoked")
        || lower.contains("invalid_grant")
        || lower.contains("refresh_token_reused")
    {
        AuthErr401::Revoked
    } else if lower.contains("token_expired") {
        AuthErr401::Expired
    } else {
        AuthErr401::Unknown
    }
}

/// Single-flight token refresher.
///
/// This struct itself holds NO token state (except the refresh mutex): the
/// caller owns the current [`OAuthTokens`] and passes them in. Persistence of
/// refreshed tokens is the caller's responsibility (T4/T5).
///
/// The internal mutex is shared via [`Arc`] so cheap clones of the manager
/// still coordinate on the same lock (single-flight across all clones).
pub struct TokenManager<F> {
    refresh_fn: F,
    refresh_margin: Duration,
    lock: Arc<tokio::sync::Mutex<RefreshCache>>,
}

impl<F> TokenManager<F>
where
    F: Fn(String) -> BoxFuture<'static, Result<OAuthTokens, String>> + Send + Sync,
{
    /// Create a manager. `refresh_margin` is how early a token is considered
    /// stale (5 minutes recommended in production; tests use smaller values).
    pub fn new(refresh_fn: F, refresh_margin: Duration) -> Self {
        Self {
            refresh_fn,
            refresh_margin,
            lock: Arc::new(tokio::sync::Mutex::new(RefreshCache::new())),
        }
    }

    /// Margin used to decide staleness (exposed for callers/tests).
    pub fn refresh_margin(&self) -> Duration {
        self.refresh_margin
    }

    fn is_expired(&self, tokens: &OAuthTokens) -> bool {
        match tokens.expires_at {
            None => false,
            Some(expires_at) => Instant::now() + self.refresh_margin >= expires_at,
        }
    }

    /// Return valid tokens, refreshing first if the given ones are stale.
    ///
    /// - Not stale (`expires_at` is `None` or beyond `now + margin`) → returns
    ///   a clone, no refresh attempted.
    /// - Stale → acquire the single-flight lock, **re-check staleness under
    ///   the lock** (a concurrent winner may have refreshed already; the
    ///   caller passes the updated tokens in, so waiters that see fresh tokens
    ///   simply reuse them), call `refresh_fn(refresh_token)`.
    /// - Stale with `refresh_token == None` → `Err("token expired and not
    ///   refreshable")`.
    /// - Refresh failure → the error is propagated verbatim; this manager
    ///   holds no token state so there is nothing to roll back (the caller's
    ///   stored tokens are untouched).
    pub async fn current(&self, tokens: &OAuthTokens) -> Result<OAuthTokens, String> {
        if !self.is_expired(tokens) {
            return Ok(tokens.clone());
        }

        let refresh_token = tokens
            .refresh_token
            .clone()
            .ok_or_else(|| "token expired and not refreshable".to_string())?;

        // Single-flight: concurrent refreshers serialize here.
        let mut cache = self.lock.lock().await;

        // Re-check under the lock: a concurrent winner that spent the same
        // refresh token has its result cached — reuse it instead of spending
        // that (single-use) refresh token a second time, which vendors treat
        // as `refresh_token_reused` and revoke the whole sign-in for.
        if let Some(fresh) = cache.get(&refresh_token) {
            return Ok(fresh.clone());
        }

        let fresh = (self.refresh_fn)(refresh_token.clone()).await?;
        cache.put(refresh_token, fresh.clone());
        Ok(fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Deterministic expired/valid token pair.
    fn expired_tokens() -> OAuthTokens {
        OAuthTokens {
            access_token: "old-access".to_string(),
            refresh_token: Some("refresh-1".to_string()),
            expires_at: Some(Instant::now() - Duration::from_secs(60)),
        }
    }

    fn valid_tokens() -> OAuthTokens {
        OAuthTokens {
            access_token: "fresh-access".to_string(),
            refresh_token: Some("refresh-1".to_string()),
            expires_at: Some(Instant::now() + Duration::from_secs(3600)),
        }
    }

    fn refreshed_tokens() -> OAuthTokens {
        OAuthTokens {
            access_token: "new-access".to_string(),
            refresh_token: Some("refresh-2".to_string()),
            expires_at: Some(Instant::now() + Duration::from_secs(3600)),
        }
    }

    #[tokio::test]
    async fn valid_tokens_skip_refresh() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fn = Arc::clone(&calls);
        let manager = TokenManager::new(
            move |_refresh_token: String| {
                let calls = Arc::clone(&calls_for_fn);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(refreshed_tokens())
                })
            },
            Duration::from_secs(300),
        );

        let tokens = valid_tokens();
        let got = manager.current(&tokens).await.expect("valid tokens ok");
        assert_eq!(got, tokens);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "refresh_fn must not run");
    }

    #[tokio::test]
    async fn expired_tokens_refresh_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fn = Arc::clone(&calls);
        let manager = TokenManager::new(
            move |_refresh_token: String| {
                let calls = Arc::clone(&calls_for_fn);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(refreshed_tokens())
                })
            },
            Duration::from_secs(300),
        );

        let got = manager
            .current(&expired_tokens())
            .await
            .expect("refresh ok");
        assert_eq!(got.access_token, refreshed_tokens().access_token);
        assert_eq!(got.refresh_token, refreshed_tokens().refresh_token);
        assert!(got.expires_at.unwrap() > Instant::now());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_current_refreshes_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fn = Arc::clone(&calls);
        let refresh_results = Arc::new(tokio::sync::Mutex::new(Vec::<OAuthTokens>::new()));
        let manager = Arc::new(TokenManager::new(
            move |_refresh_token: String| {
                let calls = Arc::clone(&calls_for_fn);
                let results = Arc::clone(&refresh_results);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let fresh = refreshed_tokens();
                    results.lock().await.push(fresh.clone());
                    // Hold the "network call" long enough for all 10
                    // waiters to pile up on the single-flight lock.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(fresh)
                })
            },
            Duration::from_secs(300),
        ));

        // 10 concurrent `current` calls all starting from the same expired
        // tokens (no shared mutable state between them).
        let mut handles = Vec::new();
        for _ in 0..10 {
            let manager = Arc::clone(&manager);
            handles.push(tokio::spawn(async move {
                manager.current(&expired_tokens()).await
            }));
        }

        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.expect("task join").expect("current ok"));
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one refresh");
        for tokens in &results {
            assert_eq!(
                tokens.access_token,
                refreshed_tokens().access_token,
                "all callers get new tokens"
            );
            assert_eq!(tokens.refresh_token, refreshed_tokens().refresh_token);
        }
    }

    #[tokio::test]
    async fn second_caller_reuses_after_lock_wait() {
        // Caller A refreshes first (sequentially, lock free). Caller B then
        // calls `current` with the tokens A produced: they are fresh, so no
        // second refresh happens even without lock contention.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fn = Arc::clone(&calls);
        let manager = TokenManager::new(
            move |_refresh_token: String| {
                let calls = Arc::clone(&calls_for_fn);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(refreshed_tokens())
                })
            },
            Duration::from_secs(300),
        );

        // Simulate: refresh happened, caller persisted the result, then a
        // concurrent caller that was waiting on the lock observes the new
        // tokens (passed in via its own view of shared state) and reuses them.
        let fresh = manager.current(&expired_tokens()).await.expect("first ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let got = manager.current(&fresh).await.expect("second ok");
        assert_eq!(got, fresh);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "recheck reuses fresh tokens"
        );
    }

    #[tokio::test]
    async fn recheck_under_lock_skips_refresh_after_wait() {
        // Directly exercise the under-lock recheck path: a task refreshes,
        // and a second task that started with expired tokens (queued on the
        // lock) receives the refreshed tokens via the caller-held state and
        // must NOT call refresh_fn again.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fn = Arc::clone(&calls);
        let manager = Arc::new(TokenManager::new(
            move |_refresh_token: String| {
                let calls = Arc::clone(&calls_for_fn);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    Ok(refreshed_tokens())
                })
            },
            Duration::from_secs(300),
        ));

        // Shared caller-held token state, like a real credential store.
        let shared = Arc::new(tokio::sync::Mutex::new(expired_tokens()));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let manager = Arc::clone(&manager);
            let shared = Arc::clone(&shared);
            handles.push(tokio::spawn(async move {
                // Re-read latest tokens right before deciding (mimics T4
                // reading the store then asking the manager).
                let snapshot = { shared.lock().await.clone() };
                let fresh = manager.current(&snapshot).await?;
                *shared.lock().await = fresh.clone();
                Ok::<OAuthTokens, String>(fresh)
            }));
        }

        for handle in handles {
            let got = handle.await.expect("join").expect("current ok");
            assert_eq!(got.access_token, refreshed_tokens().access_token);
            assert_eq!(got.refresh_token, refreshed_tokens().refresh_token);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "lock recheck prevents 2nd refresh"
        );
    }

    #[tokio::test]
    async fn refresh_error_propagates() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fn = Arc::clone(&calls);
        let manager = TokenManager::new(
            move |_refresh_token: String| {
                let calls = Arc::clone(&calls_for_fn);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err("upstream 400".to_string())
                })
            },
            Duration::from_secs(300),
        );

        let err = manager
            .current(&expired_tokens())
            .await
            .expect_err("error propagates");
        assert_eq!(err, "upstream 400");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn expired_without_refresh_token_errors() {
        let calls = Arc::new(AtomicUsize::new(0));
        let manager = TokenManager::new(
            |_refresh_token: String| {
                Box::pin(async move {
                    // Must never be reached.
                    Err("unreachable".to_string())
                })
            },
            Duration::from_secs(300),
        );

        let tokens = OAuthTokens {
            access_token: "old-access".to_string(),
            refresh_token: None,
            expires_at: Some(Instant::now() - Duration::from_secs(60)),
        };
        let err = manager.current(&tokens).await.expect_err("not refreshable");
        assert_eq!(err, "token expired and not refreshable");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn classify_401_markers() {
        assert_eq!(classify_401("token_revoked"), AuthErr401::Revoked);
        assert_eq!(classify_401("invalid_grant"), AuthErr401::Revoked);
        assert_eq!(classify_401("refresh_token_reused"), AuthErr401::Revoked);
        assert_eq!(classify_401("token_expired"), AuthErr401::Expired);
        // Substring matching is the documented contract (magpie matches
        // these markers inside larger JSON error bodies); a marker with a
        // suffix is still matched, which is intended.
        assert_eq!(classify_401("token_revokedx"), AuthErr401::Revoked);
        assert_eq!(classify_401("unauthorized"), AuthErr401::Unknown);
        assert_eq!(classify_401(""), AuthErr401::Unknown);
    }

    #[test]
    fn classify_401_case_insensitive() {
        assert_eq!(classify_401("TOKEN_EXPIRED"), AuthErr401::Expired);
        assert_eq!(classify_401("Invalid_Grant"), AuthErr401::Revoked);
        assert_eq!(classify_401("REFRESH_TOKEN_REUSED"), AuthErr401::Revoked);
    }

    #[test]
    fn classify_401_json_body() {
        // Realistic shapes: markers embedded in JSON payloads.
        assert_eq!(
            classify_401(r#"{"error":"invalid_grant","error_description":"Token expired"}"#),
            AuthErr401::Revoked
        );
        assert_eq!(
            classify_401(r#"{"error":{"code":"token_expired"}}"#),
            AuthErr401::Expired
        );
    }
}
