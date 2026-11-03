//! What a failed attempt means: which 401s are a spent token, which are a
//! revoked sign-in, and which are somebody else's problem. No I/O of its
//! own — the account store it touches is the resolver's.

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use http_body_util::BodyExt;
use log::{debug, info, warn};

use crate::credentials::token::{AuthErr401, classify_401};

use super::super::api_error_response;
use super::{AppState, RequestContext};

/// Failover-loop action for one provider attempt after an account-aware
/// 401/403.
pub(crate) enum AuthStep {
    /// Retry the same provider without consuming failover quota.
    RetrySame,
    /// Move to the next provider.
    Next,
    /// No account semantics applied: keep the existing status handling.
    FallThrough,
}

/// Account-aware handling of an upstream 401/403 for providers with
/// `account` set: one refresh on `token_expired`, a persisted marker in
/// `revoked.json` on `token_revoked` / `invalid_grant` /
/// `refresh_token_reused`, and the corresponding loop step. An Unknown
/// 401 moves on when another account/provider target remains (P9).
///
/// Only the account id and the verdict are logged — never the response
/// body, which can echo tokens. `Err(response)` ends the request right
/// here (revoked account without another target).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn classify_account_auth(
    state: &AppState,
    ctx: &RequestContext,
    provider: &crate::config::ProviderConfig,
    account_id: &str,
    text: &str,
    already_retried: bool,
    attempt: usize,
    has_next: bool,
    expired_retried: &mut Option<usize>,
    last_error: &mut Option<String>,
) -> Result<AuthStep, Response> {
    let step = match classify_401(text) {
        AuthErr401::Expired if !already_retried => {
            info!(
                "upstream returned token_expired request_id={} provider={} account={account_id}, refreshing once",
                ctx.req_id, provider.name
            );
            match state.accounts.force_refresh(account_id).await {
                Ok(_) => AuthStep::RetrySame,
                Err(err) => {
                    warn!(
                        "refresh after token_expired failed request_id={} provider={} account={account_id} error={err}, trying next account/provider",
                        ctx.req_id, provider.name
                    );
                    *last_error = Some(format!(
                        "provider {} account {account_id}: refresh after token_expired failed",
                        provider.name
                    ));
                    AuthStep::Next
                }
            }
        }
        AuthErr401::Expired => {
            // The single retry for this attempt was already spent.
            debug!(
                "token_expired retry already consumed request_id={} provider={} account={account_id}",
                ctx.req_id, provider.name
            );
            AuthStep::FallThrough
        }
        AuthErr401::Revoked => {
            warn!(
                "upstream revoked credentials request_id={} provider={} account={account_id}, marking account revoked",
                ctx.req_id, provider.name
            );
            if let Err(err) = state.accounts.mark_revoked(account_id, text).await {
                warn!(
                    "failed to persist revoked marker request_id={} account={account_id} error={err}",
                    ctx.req_id
                );
            }
            *last_error = Some(format!(
                "provider {} account {account_id} revoked",
                provider.name
            ));
            if !has_next {
                return Err(revoked_account_response(account_id));
            }
            AuthStep::Next
        }
        AuthErr401::Unknown => {
            // No account semantics apply; with another target left (P9)
            // the next account/provider takes the request, otherwise the
            // upstream's own answer goes back unchanged.
            if has_next {
                AuthStep::Next
            } else {
                AuthStep::FallThrough
            }
        }
    };

    match step {
        AuthStep::RetrySame => {
            *expired_retried = Some(attempt);
            Ok(AuthStep::RetrySame)
        }
        AuthStep::Next => Ok(AuthStep::Next),
        AuthStep::FallThrough => Ok(AuthStep::FallThrough),
    }
}

/// Split a fully-buffered error [`Response`] into its body text plus a
/// rebuilt response, so the failover loop can classify 401 bodies while
/// still returning the original response unchanged when nothing applies.
pub(crate) async fn take_error_body(response: Response) -> (Response, String) {
    let (parts, body) = response.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => axum::body::Bytes::new(),
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (Response::from_parts(parts, Body::from(bytes)), text)
}

/// The unique provider's account was just revoked: the client must sign in
/// again, so say so instead of a generic gateway error.
pub(crate) fn revoked_account_response(account_id: &str) -> Response {
    api_error_response(
        StatusCode::BAD_GATEWAY,
        "account_revoked",
        format!(
            "account {account_id} was revoked by the upstream provider — sign in again to continue (`any-converter accounts add`)"
        ),
    )
}

pub(crate) fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504 | 529)
}
