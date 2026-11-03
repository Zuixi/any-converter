//! Account quota windows (P9), mirroring magpie's `internal/provider/
//! allowance*.go`: a vendor reports [`Limit`] windows, an [`Allowance`]
//! collects them, and routing asks how much of them is used.
//!
//! Two layers are kept apart on purpose (magpie `account_order.go` vs
//! `allowance*.go`): the user's account order is authoritative, and quota
//! only moves accounts *past their line* to the back — it never reorders
//! them (see [`crate::provider_pool`]).
//!
//! Reading the windows is [`AccountResolver::allowance`] (Gemini /
//! Antigravity, magpie `modelInfo`'s Antigravity branch) and
//! [`copilot_allowance`] (magpie `CopilotUserURL`); every other vendor
//! reports nothing and its accounts count as unused.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::account::{AccountError, AccountResolver};
use super::copilot::{COPILOT_DEFAULT_API_BASE, CopilotSession, copilot_api_headers};

/// One window of an account's allowance (magpie provider.Limit).
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    /// Percent of the window used, 0–100.
    pub used: f64,
    /// When the window resets; `None` when the vendor doesn't say.
    pub resets: Option<SystemTime>,
    /// How long the window runs; zero when unknown.
    pub span: Duration,
    /// Counts only models whose id contains this; `None` counts every
    /// model (magpie's `strings.Contains(model, limit.Model)`).
    pub model: Option<String>,
}

impl Limit {
    /// Whether this window counts requests for `model`.
    fn counts(&self, model: &str) -> bool {
        match &self.model {
            Some(word) => model.contains(word.as_str()),
            None => true,
        }
    }

    /// The window's used percent at `now`: a window whose reset has passed
    /// is empty again and reads 0 (magpie `For`).
    fn used_now(&self, now: SystemTime) -> f64 {
        match self.resets {
            Some(resets) if resets <= now => 0.0,
            _ => self.used,
        }
    }
}

/// Every window an account reported (magpie provider.Allowance).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Allowance(Vec<Limit>);

impl Allowance {
    pub fn new(limits: Vec<Limit>) -> Self {
        Self(limits)
    }

    /// Whether the account reported no windows at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The account's windows, in the vendor's order.
    pub fn iter(&self) -> impl Iterator<Item = &Limit> {
        self.0.iter()
    }

    /// The windows related to `model`: every window that counts requests
    /// for it ([`Limit::counts`]). Expiry is not filtered here — a window
    /// whose reset has passed reads 0 through [`Self::fullest_used`] and
    /// [`Self::exhausted`] (magpie `For`'s rule).
    pub fn for_model(&self, model: &str, _now: SystemTime) -> Vec<&Limit> {
        self.0.iter().filter(|limit| limit.counts(model)).collect()
    }

    /// Used percent of the fullest window that counts for the model, with
    /// a window whose reset has passed read as 0 (magpie `For`'s used).
    pub fn fullest_used(&self, model: &str, now: SystemTime) -> f64 {
        self.for_model(model, now)
            .iter()
            .map(|limit| limit.used_now(now))
            .fold(0.0, f64::max)
    }

    /// Whether the account is past `share` on every window counting for
    /// it: at least one window counts for the model and each of them
    /// reads `share` percent used or more at `now`. An account with no
    /// matching window is not exhausted — nothing says it is.
    pub fn exhausted(&self, model: &str, share: f64, now: SystemTime) -> bool {
        let windows = self.for_model(model, now);
        !windows.is_empty() && windows.iter().all(|limit| limit.used_now(now) >= share)
    }
}

impl AccountResolver {
    /// The account's allowance windows, when its vendor tells them
    /// (Gemini / Antigravity via `POST {base}/v1internal:fetchAvailableModels`,
    /// magpie `modelInfo`'s Antigravity branch; other providers report
    /// nothing and answer `None`). Cached 5 minutes; a failed read keeps
    /// the last known value.
    pub async fn allowance(&self, account_id: &str, base: &str) -> Option<Allowance> {
        let provider = account_id.split(':').next().unwrap_or("");
        if provider != "gemini" && provider != "antigravity" {
            return None;
        }
        if let Some((at, allowance)) = self.allowance_cache_get(account_id) {
            if unix_now() - at < ALLOWANCE_CACHE_SECS {
                return Some(allowance);
            }
        }
        match self.read_allowance(account_id, base).await {
            Ok(allowance) => {
                self.allowance_cache_put(account_id, allowance.clone());
                Some(allowance)
            }
            Err(err) => {
                log::warn!(
                    "allowance read failed for account {account_id}: {err}; \
                     keeping the last known windows"
                );
                self.allowance_cache_get(account_id)
                    .map(|(_, allowance)| allowance)
            }
        }
    }

    /// The windows read earlier, if any and still fresh — **no I/O**.
    ///
    /// This is what routing uses: magpie reads allowances in the background
    /// and never makes a request wait on one (`KeyAllowance`: "never waited
    /// for, read in the background"). An account with nothing cached counts
    /// as having room, which is why a cold start keeps the user's order.
    pub(crate) fn allowance_fresh(&self, account_id: &str) -> Option<Allowance> {
        let (at, allowance) = self.allowance_cache_get(account_id)?;
        (unix_now() - at < ALLOWANCE_CACHE_SECS).then_some(allowance)
    }

    /// Refresh `account_id`'s windows in the background, unless a read is
    /// already in flight for it or the cached value is still fresh. The
    /// request that noticed staleness is never held up by the read.
    pub(crate) fn refresh_allowance_in_background(self: &Arc<Self>, account_id: &str, base: &str) {
        if self.allowance_fresh(account_id).is_some() {
            return;
        }
        {
            let mut reading = self.reading.lock().unwrap_or_else(|e| e.into_inner());
            if !reading.insert(account_id.to_string()) {
                return; // one read at a time per account, as magpie keeps it
            }
        }
        let this = Arc::clone(self);
        let account_id = account_id.to_string();
        let base = base.to_string();
        tokio::spawn(async move {
            let _ = this.allowance(&account_id, &base).await;
            this.reading
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&account_id);
        });
    }

    /// One `fetchAvailableModels` ask, signed with the account's access
    /// token. The project the account's requests go to rides in the body
    /// when an earlier `loadCodeAssist` cached one; a quota read never
    /// onboards the account itself.
    async fn read_allowance(
        &self,
        account_id: &str,
        base: &str,
    ) -> Result<Allowance, AccountError> {
        let account = self.resolve(account_id).await?;
        let body = match self.cached_project_id(account_id) {
            Some(project) => serde_json::json!({ "project": project }),
            None => serde_json::json!({}),
        };
        let url = format!(
            "{}/v1internal:fetchAvailableModels",
            base.trim_end_matches('/')
        );
        let response = self
            .http_client()
            .post(&url)
            .bearer_auth(account.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|error| AccountError::CodeAssist(format!("{url} request failed: {error}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| AccountError::CodeAssist(format!("{url} read failed: {error}")))?;
        if !status.is_success() {
            return Err(AccountError::CodeAssist(format!(
                "{url} returned {status}: {text}"
            )));
        }
        let reply: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
            AccountError::CodeAssist(format!("{url} returned malformed payload: {error}"))
        })?;
        Ok(gemini_allowance_from_reply(&reply))
    }
}

/// How long a quota reading stays fresh (P9: cached 5 minutes).
pub(crate) const ALLOWANCE_CACHE_SECS: i64 = 300;

/// Parse a Gemini / Antigravity `fetchAvailableModels` reply into windows
/// (magpie `modelInfo`'s Antigravity branch): `models` maps a model id to
/// `{quotaInfo: {remainingFraction, resetTime}}`; used is
/// `1 - remainingFraction` as a percent, the reset is RFC3339, and the
/// window counts only that model id (the full id — magpie's `Limit.Model`
/// is a "word in the id" match, here it is the exact id Google named).
/// The window's span is unknown → [`Duration::ZERO`].
pub(crate) fn gemini_allowance_from_reply(reply: &serde_json::Value) -> Allowance {
    let mut limits = Vec::new();
    let Some(models) = reply.get("models").and_then(serde_json::Value::as_object) else {
        return Allowance::new(limits);
    };
    for (id, detail) in models {
        let Some(quota) = detail
            .get("quotaInfo")
            .and_then(serde_json::Value::as_object)
        else {
            // No quota window reported for this model (magpie keeps the
            // remaining at -1 there: unknown, no window).
            continue;
        };
        // Antigravity omits remainingFraction once none is left (magpie
        // modelInfo): the window then reads fully used.
        let remaining = quota
            .get("remainingFraction")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let used = ((1.0 - remaining) * 100.0).clamp(0.0, 100.0);
        let resets = quota
            .get("resetTime")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_rfc3339);
        limits.push(Limit {
            used,
            resets,
            span: Duration::ZERO,
            model: Some(id.clone()),
        });
    }
    Allowance::new(limits)
}

/// Read a Copilot account's allowance: `GET {session.api_base}/
/// copilot_internal/user` with the session's Copilot headers (magpie
/// `CopilotUserURL`); `quota_snapshots.premium_interactions`
/// (`percent_remaining` / `unlimited`) becomes one account-wide window.
/// `None` when the vendor doesn't answer, or the snapshot is missing.
pub async fn copilot_allowance(
    client: &reqwest::Client,
    session: &CopilotSession,
) -> Option<Allowance> {
    let base = if session.api_base.is_empty() {
        COPILOT_DEFAULT_API_BASE
    } else {
        session.api_base.as_str()
    };
    let url = format!("{}/copilot_internal/user", base.trim_end_matches('/'));
    let mut request = client.get(&url);
    for (name, value) in copilot_api_headers(&session.token) {
        request = request.header(name, value);
    }
    let response = request.send().await.ok()?;
    let status = response.status();
    let text = response.text().await.ok()?;
    if !status.is_success() {
        log::warn!("copilot quota read {url} returned {status}: {text}");
        return None;
    }
    let user: serde_json::Value = match serde_json::from_str(&text) {
        Ok(user) => user,
        Err(err) => {
            log::warn!("copilot quota read {url} returned malformed payload: {err}");
            return None;
        }
    };
    match copilot_allowance_from_user(&user) {
        Some(allowance) => Some(allowance),
        None => {
            log::warn!("copilot quota read {url}: no premium_interactions snapshot");
            None
        }
    }
}

/// The `quota_snapshots.premium_interactions` window of a
/// `copilot_internal/user` reply: one account-wide window whose used is
/// `1 - percent_remaining` as a percent. `None` when the snapshot is
/// missing; an `unlimited` snapshot reads as no window at all.
pub(crate) fn copilot_allowance_from_user(user: &serde_json::Value) -> Option<Allowance> {
    let snapshot = user
        .get("quota_snapshots")
        .and_then(serde_json::Value::as_object)?
        .get("premium_interactions")?;
    if snapshot
        .get("unlimited")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Some(Allowance::new(Vec::new()));
    }
    let percent = snapshot
        .get("percent_remaining")
        .and_then(serde_json::Value::as_f64)?;
    let used = ((1.0 - percent) * 100.0).clamp(0.0, 100.0);
    Some(Allowance::new(vec![Limit {
        used,
        resets: None,
        span: Duration::ZERO,
        model: None,
    }]))
}

/// RFC3339 → [`SystemTime`].
fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    let parsed = chrono::DateTime::parse_from_rfc3339(text).ok()?;
    let secs = parsed.timestamp().max(0) as u64;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const NOW: SystemTime = SystemTime::UNIX_EPOCH;

    fn limit(used: f64, resets: Option<SystemTime>, model: Option<&str>) -> Limit {
        Limit {
            used,
            resets,
            span: Duration::ZERO,
            model: model.map(String::from),
        }
    }

    /// A reset in the future / past of the test `now`.
    fn resets_in(secs: i64) -> Option<SystemTime> {
        if secs >= 0 {
            NOW.checked_add(Duration::from_secs(secs as u64))
        } else {
            NOW.checked_sub(Duration::from_secs(secs.unsigned_abs()))
        }
    }

    #[test]
    fn test_for_model_filters_windows_by_model_word() {
        let allowance = Allowance::new(vec![
            limit(10.0, None, None),
            limit(20.0, None, Some("gemini")),
            limit(30.0, None, Some("claude")),
        ]);
        let gemini_windows = allowance.for_model("gemini-3.5-flash", NOW);
        assert_eq!(gemini_windows.len(), 2);
        assert_eq!(gemini_windows[0].used, 10.0);
        assert_eq!(gemini_windows[1].used, 20.0);

        // A window names a word that must appear in the model id.
        let claude_windows = allowance.for_model("claude-opus-4", NOW);
        assert_eq!(claude_windows.len(), 2);
        assert_eq!(claude_windows[1].used, 30.0);
    }

    #[test]
    fn test_fullest_used_of_empty_allowance_is_zero() {
        let allowance = Allowance::new(vec![]);
        assert_eq!(allowance.fullest_used("gemini-3.5-flash", NOW), 0.0);
    }

    #[test]
    fn test_fullest_used_reads_passed_reset_as_zero() {
        let allowance = Allowance::new(vec![
            limit(99.0, resets_in(-1), None), // reset passed: reads 0
            limit(20.0, resets_in(3600), None),
        ]);
        assert_eq!(allowance.fullest_used("anything", NOW), 20.0);
    }

    #[test]
    fn test_fullest_used_ignores_windows_of_other_models() {
        let allowance = Allowance::new(vec![
            limit(99.0, resets_in(3600), Some("gemini")),
            limit(10.0, resets_in(3600), Some("claude")),
        ]);
        assert_eq!(allowance.fullest_used("gemini-3.5-flash", NOW), 99.0);
        assert_eq!(allowance.fullest_used("claude-opus-4", NOW), 10.0);
        assert_eq!(allowance.fullest_used("other", NOW), 0.0);
    }

    #[test]
    fn test_exhausted_empty_allowance_is_not_exhausted() {
        let allowance = Allowance::new(vec![]);
        assert!(!allowance.exhausted("gemini-3.5-flash", 95.0, NOW));
    }

    #[test]
    fn test_exhausted_boundary_equal_to_share_counts() {
        let allowance = Allowance::new(vec![limit(95.0, resets_in(3600), None)]);
        assert!(
            allowance.exhausted("gemini-3.5-flash", 95.0, NOW),
            "reaching the share is exhausted"
        );
        assert!(!allowance.exhausted("gemini-3.5-flash", 95.1, NOW));
    }

    #[test]
    fn test_exhausted_when_every_window_is_past_the_share() {
        let allowance = Allowance::new(vec![
            limit(96.0, resets_in(3600), None),
            limit(99.0, resets_in(7200), Some("gemini")),
        ]);
        assert!(allowance.exhausted("gemini-3.5-flash", 95.0, NOW));
    }

    #[test]
    fn test_exhausted_when_only_one_window_is_over() {
        // One window over the share is not enough: the account still has
        // room in the other window counting for the model.
        let allowance = Allowance::new(vec![
            limit(99.0, resets_in(3600), Some("gemini")),
            limit(50.0, resets_in(7200), Some("gemini")),
        ]);
        assert!(!allowance.exhausted("gemini-3.5-flash", 95.0, NOW));
        // An expired window reads 0: past the line once, it leaves room.
        let allowance = Allowance::new(vec![
            limit(99.0, resets_in(-1), None),
            limit(50.0, resets_in(7200), None),
        ]);
        assert!(!allowance.exhausted("anything", 95.0, NOW));
    }

    #[test]
    fn test_gemini_allowance_from_reply() {
        let reply = serde_json::json!({
            "models": {
                "gemini-3.5-flash": {
                    "quotaInfo": {
                        "remainingFraction": 0.58,
                        "resetTime": "2099-01-01T00:00:00Z"
                    }
                },
                "gemini-3.5-flash-lite": {
                    "quotaInfo": { "remainingFraction": 1.0 }
                },
                "no-quota-model": {}
            }
        });
        let allowance = gemini_allowance_from_reply(&reply);
        assert_eq!(allowance.iter().count(), 2);

        let flash = allowance
            .for_model("gemini-3.5-flash", NOW)
            .into_iter()
            .find(|window| window.model.as_deref() == Some("gemini-3.5-flash"))
            .unwrap();
        assert!((flash.used - 42.0).abs() < 1e-9, "used = {}", flash.used);
        assert!(flash.resets.is_some());
        assert_eq!(flash.span, Duration::ZERO);

        let lite = allowance
            .for_model("gemini-3.5-flash-lite", NOW)
            .into_iter()
            .find(|window| window.model.as_deref() == Some("gemini-3.5-flash-lite"))
            .unwrap();
        assert_eq!(lite.used, 0.0);
        assert_eq!(lite.resets, None);
    }

    #[test]
    fn test_gemini_allowance_missing_fraction_reads_used_up() {
        let reply = serde_json::json!({
            "models": { "gemini-3.5-flash": { "quotaInfo": {} } }
        });
        let allowance = gemini_allowance_from_reply(&reply);
        let windows = allowance.for_model("gemini-3.5-flash", NOW);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used, 100.0);
    }

    #[test]
    fn test_gemini_allowance_unknown_shape_is_empty() {
        assert!(gemini_allowance_from_reply(&serde_json::json!({})).is_empty());
        assert!(gemini_allowance_from_reply(&serde_json::json!({"models": [1, 2]})).is_empty());
    }

    #[test]
    fn test_copilot_allowance_from_user() {
        let user = serde_json::json!({
            "quota_snapshots": {
                "premium_interactions": { "percent_remaining": 0.42, "unlimited": false }
            }
        });
        let allowance = copilot_allowance_from_user(&user).unwrap();
        let windows = allowance.for_model("gpt-5", NOW);
        assert_eq!(windows.len(), 1);
        let window = windows[0];
        assert!((window.used - 58.0).abs() < 1e-9, "used = {}", window.used);
        assert_eq!(window.model, None, "counts every model");
        assert_eq!(window.resets, None);

        // Missing snapshot → None.
        assert!(
            copilot_allowance_from_user(&serde_json::json!({
                "quota_snapshots": {}
            }))
            .is_none()
        );

        // Unlimited → no window at all (nothing to exhaust).
        let unlimited = copilot_allowance_from_user(&serde_json::json!({
            "quota_snapshots": {
                "premium_interactions": { "percent_remaining": 1.0, "unlimited": true }
            }
        }))
        .unwrap();
        assert!(unlimited.is_empty());
    }

    #[test]
    fn test_parse_rfc3339() {
        let at = parse_rfc3339("2026-11-03T12:00:00Z").unwrap();
        let secs = at.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        assert!(secs > 1_700_000_000, "secs = {secs}");
        assert!(parse_rfc3339("not a time").is_none());
    }
}

/// Copilot accounts' quota windows, cached so routing never waits on the
/// read: asking needs a session exchange plus a round trip, far too much to
/// put in front of every request (magpie reads allowances in the background
/// for the same reason).
#[derive(Default)]
pub struct CopilotAllowances {
    entries: std::sync::Mutex<std::collections::HashMap<String, (i64, Allowance)>>,
    reading: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl CopilotAllowances {
    /// The windows read earlier, if any and still fresh — **no I/O**.
    pub fn fresh(&self, account_id: &str) -> Option<Allowance> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (at, allowance) = entries.get(account_id)?;
        (unix_now() - at < ALLOWANCE_CACHE_SECS).then(|| allowance.clone())
    }

    /// Refresh `account_id`'s windows off the request path, one read at a
    /// time per account.
    pub fn refresh_in_background(
        self: &Arc<Self>,
        accounts: Arc<super::account::AccountResolver>,
        copilot: Arc<super::copilot::CopilotSessions>,
        client: reqwest::Client,
        account_id: String,
    ) {
        {
            let mut reading = self.reading.lock().unwrap_or_else(|e| e.into_inner());
            if !reading.insert(account_id.clone()) {
                return; // one read at a time per account
            }
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            if let Ok(github) = accounts.resolve(&account_id).await {
                if let Ok(session) = copilot.session(&github.access_token).await {
                    if let Some(allowance) = copilot_allowance(&client, &session).await {
                        this.entries
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(account_id.clone(), (unix_now(), allowance));
                    }
                }
            }
            this.reading
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&account_id);
        });
    }
}
