//! GitHub Copilot device-code sign-in (RFC 8628 against GitHub's OAuth).
//!
//! Flow (mirrors magpie `copilot_signin.go`):
//!
//! 1. [`CopilotSignIn::start`] posts `client_id` + `scope=read:user` to the
//!    device endpoint and receives a `device_code` plus a `user_code` the
//!    user enters at the verification page.
//! 2. The user opens the verification page and enters the code.
//! 3. [`CopilotSignIn::wait`] polls the token endpoint every `interval`
//!    seconds: `authorization_pending` keeps waiting, `slow_down` grows the
//!    interval by 5 seconds (RFC 8628 §3.5), `expired_token` /
//!    `access_denied` fail the flow, and an `access_token` completes it.
//!
//! All network interaction goes through the [`DeviceHttp`] trait so unit
//! tests inject a fake layer; endpoints default to GitHub's public Copilot
//! OAuth client. Nothing is persisted here — persisting the resulting
//! [`CopilotAccount`] (e.g. as a `Credential::OAuth`) is the caller's job.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::future::BoxFuture;

pub use super::gemini::SignInError;

/// Copilot OAuth client id (magpie / GitHub Copilot plugin source).
pub const COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// Device-code endpoint.
pub const COPILOT_DEVICE_URL: &str = "https://github.com/login/device/code";
/// Token endpoint (polled until the flow completes).
pub const COPILOT_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// Page where the user enters the `user_code`.
pub const COPILOT_VERIFICATION_URI: &str = "https://github.com/login/device";
/// Scope requested for the device flow.
pub const COPILOT_DEVICE_SCOPE: &str = "read:user";

/// `grant_type` for device-flow token polling (RFC 8628 §3.4).
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// RFC 8628 §3.5: on `slow_down` the poll interval must grow by 5 seconds.
const DEFAULT_SLOW_DOWN_INCREMENT: Duration = Duration::from_secs(5);
/// RFC 8628: the poll interval defaults to 5 seconds when the device
/// response omits it.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Production floor on the effective poll sleep so a bogus tiny `interval`
/// from the endpoint cannot hammer the token endpoint. Tests set it to zero
/// to accelerate polling.
const DEFAULT_POLL_FLOOR: Duration = Duration::from_secs(1);

/// GitHub OAuth endpoints for the device flow.
///
/// Injectable so tests (and future providers) can point at fake endpoints;
/// [`Default`] yields the public Copilot OAuth client.
#[derive(Debug, Clone)]
pub struct CopilotEndpoints {
    /// Device-code issuance endpoint (`POST` form).
    pub device_url: String,
    /// Token endpoint polled for the result.
    pub token_url: String,
    /// Page where the user enters the `user_code`.
    pub verification_uri: String,
    /// OAuth client id.
    pub client_id: String,
}

impl Default for CopilotEndpoints {
    fn default() -> Self {
        Self {
            device_url: COPILOT_DEVICE_URL.to_string(),
            token_url: COPILOT_TOKEN_URL.to_string(),
            verification_uri: COPILOT_VERIFICATION_URI.to_string(),
            client_id: COPILOT_CLIENT_ID.to_string(),
        }
    }
}

/// Everything [`CopilotSignIn::start`] learned from the device endpoint.
#[derive(Debug, Clone)]
pub struct DeviceCodeStarted {
    /// Code the user enters at the verification page (e.g. `WDJB-MJHT`).
    pub user_code: String,
    /// Page where the user enters the code.
    pub verification_uri: String,
    /// Secret handle polled at the token endpoint.
    pub device_code: String,
    /// Suggested poll interval (the flow may grow it via `slow_down`).
    pub interval: Duration,
}

/// A fully signed-in Copilot device-flow account.
///
/// Only the access token is captured; resolving the GitHub user (and
/// exchanging for a Copilot token) is a later task's job. Persisting this
/// (e.g. via [`super::FileSecretStore`]) is the caller's job.
#[derive(Debug, Clone)]
pub struct CopilotAccount {
    /// Masked hint of the `user_code` used as the account label
    /// (e.g. `WDJB-MJHT` → `wdjb****`).
    pub user_code_hint: String,
    /// GitHub OAuth access token.
    pub access_token: String,
}

// ---------------------------------------------------------------------------
// Injectable HTTP layer
// ---------------------------------------------------------------------------

/// One device-code request, fully owned (the injected layer produces
/// `'static` futures).
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCodeRequest {
    pub device_url: String,
    pub client_id: String,
    pub scope: String,
}

/// Device endpoint response (`expires_in` is intentionally not captured —
/// the caller bounds the wait with an explicit timeout).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    #[serde(default)]
    pub verification_uri: Option<String>,
    #[serde(default)]
    pub interval: Option<u64>,
}

/// One token-endpoint poll, fully owned.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenPollRequest {
    pub token_url: String,
    pub client_id: String,
    pub device_code: String,
    pub grant_type: String,
}

/// Token endpoint payload: either an `access_token`, a flow `error`
/// (`authorization_pending` / `slow_down` / `expired_token` /
/// `access_denied`), or neither (malformed response).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct TokenPollResponse {
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Injectable network layer for the two device-flow steps.
///
/// Production uses the reqwest implementation below; tests inject fakes so
/// unit tests never touch the network. Methods return boxed futures
/// (object-safe) instead of requiring an `async_trait` dependency.
pub trait DeviceHttp: Send + Sync {
    /// Request a device code from the device endpoint.
    fn request_device_code(
        &self,
        request: DeviceCodeRequest,
    ) -> BoxFuture<'static, Result<DeviceCodeResponse, SignInError>>;

    /// Poll the token endpoint once for the flow result.
    fn poll_token(
        &self,
        request: TokenPollRequest,
    ) -> BoxFuture<'static, Result<TokenPollResponse, SignInError>>;
}

/// Production [`DeviceHttp`] backed by reqwest.
///
/// Posts `application/x-www-form-urlencoded` bodies with
/// `Accept: application/json` (GitHub returns form-encoded data otherwise).
pub struct ReqwestDeviceHttp {
    client: reqwest::Client,
}

impl Default for ReqwestDeviceHttp {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl ReqwestDeviceHttp {
    /// Create the production HTTP layer.
    pub fn new() -> Self {
        Self::default()
    }
}

impl DeviceHttp for ReqwestDeviceHttp {
    fn request_device_code(
        &self,
        request: DeviceCodeRequest,
    ) -> BoxFuture<'static, Result<DeviceCodeResponse, SignInError>> {
        // Clone the client up front so the future captures nothing borrowed.
        let client = self.client.clone();
        Box::pin(async move {
            let body = form_urlencoded(&[
                ("client_id", request.client_id.as_str()),
                ("scope", request.scope.as_str()),
            ]);
            let response = client
                .post(&request.device_url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .header(reqwest::header::ACCEPT, "application/json")
                .body(body)
                .send()
                .await?;
            let status = response.status();
            let text = response.text().await?;
            if !status.is_success() {
                return Err(SignInError::Vendor(format!(
                    "device endpoint returned {status}: {text}"
                )));
            }
            serde_json::from_str::<DeviceCodeResponse>(&text).map_err(|e| {
                SignInError::Vendor(format!("device endpoint returned malformed payload: {e}"))
            })
        })
    }

    fn poll_token(
        &self,
        request: TokenPollRequest,
    ) -> BoxFuture<'static, Result<TokenPollResponse, SignInError>> {
        // Clone the client up front so the future captures nothing borrowed.
        let client = self.client.clone();
        Box::pin(async move {
            let body = form_urlencoded(&[
                ("grant_type", request.grant_type.as_str()),
                ("device_code", request.device_code.as_str()),
                ("client_id", request.client_id.as_str()),
            ]);
            let response = client
                .post(&request.token_url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .header(reqwest::header::ACCEPT, "application/json")
                .body(body)
                .send()
                .await?;
            let status = response.status();
            let text = response.text().await?;
            // GitHub reports flow errors as 200 + `error` field; only
            // transport-level non-2xx lands here.
            if !status.is_success() {
                return Err(SignInError::Vendor(format!(
                    "token endpoint returned {status}: {text}"
                )));
            }
            serde_json::from_str::<TokenPollResponse>(&text).map_err(|e| {
                SignInError::Vendor(format!("token endpoint returned malformed payload: {e}"))
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Sign-in driver
// ---------------------------------------------------------------------------

/// Drives one Copilot device-code sign-in against configurable endpoints.
///
/// Usage: `start()` → user enters the printed `user_code` at the printed
/// verification URL → `wait(&started, timeout)` polls until the flow
/// completes, fails, or the timeout elapses.
pub struct CopilotSignIn {
    endpoints: CopilotEndpoints,
    http: Arc<dyn DeviceHttp>,
    /// Lower bound on the effective poll sleep (production: 1s so a bogus
    /// `interval` cannot hammer the endpoint; tests: 0 to run flat out).
    poll_floor: Duration,
    /// How much `slow_down` grows the poll interval (5s per RFC 8628;
    /// tests shrink it to keep the suite fast).
    slow_down_increment: Duration,
}

impl CopilotSignIn {
    /// Create a sign-in for the given endpoints using the real reqwest
    /// device layer.
    pub fn new(endpoints: CopilotEndpoints) -> Self {
        Self::with_http(endpoints, Arc::new(ReqwestDeviceHttp::new()))
    }

    /// Create a sign-in with an injected [`DeviceHttp`] layer (tests).
    pub fn with_http(endpoints: CopilotEndpoints, http: Arc<dyn DeviceHttp>) -> Self {
        Self {
            endpoints,
            http,
            poll_floor: DEFAULT_POLL_FLOOR,
            slow_down_increment: DEFAULT_SLOW_DOWN_INCREMENT,
        }
    }

    /// Override the poll-sleep floor (tests set zero to accelerate polling).
    pub fn with_poll_floor(&mut self, floor: Duration) -> &mut Self {
        self.poll_floor = floor;
        self
    }

    /// Override the `slow_down` interval growth (tests shrink it to keep
    /// the suite fast; production keeps the RFC-mandated 5 seconds).
    pub fn with_slow_down_increment(&mut self, increment: Duration) -> &mut Self {
        self.slow_down_increment = increment;
        self
    }

    /// Request a device code from the device endpoint.
    pub async fn start(&self) -> Result<DeviceCodeStarted, SignInError> {
        let response = self
            .http
            .request_device_code(DeviceCodeRequest {
                device_url: self.endpoints.device_url.clone(),
                client_id: self.endpoints.client_id.clone(),
                scope: COPILOT_DEVICE_SCOPE.to_string(),
            })
            .await?;

        Ok(DeviceCodeStarted {
            user_code: response.user_code,
            verification_uri: response
                .verification_uri
                .unwrap_or_else(|| self.endpoints.verification_uri.clone()),
            device_code: response.device_code,
            interval: response
                .interval
                .map_or(DEFAULT_POLL_INTERVAL, Duration::from_secs),
        })
    }

    /// Poll the token endpoint until the flow completes, fails, or `timeout`
    /// elapses.
    ///
    /// - `authorization_pending` → keep polling after `interval`
    /// - `slow_down` → `interval += 5s`, keep polling
    /// - `expired_token` → [`SignInError::Vendor`] ("the code expired;
    ///   start again")
    /// - `access_denied` → [`SignInError::Vendor`] ("the sign-in was
    ///   declined")
    /// - `access_token` present → [`CopilotAccount`]
    /// - deadline elapses → [`SignInError::Timeout`]
    pub async fn wait(
        self,
        started: &DeviceCodeStarted,
        timeout: Duration,
    ) -> Result<CopilotAccount, SignInError> {
        let deadline = Instant::now() + timeout;
        let mut interval = started.interval;
        loop {
            if Instant::now() >= deadline {
                return Err(SignInError::Timeout);
            }

            let response = self
                .http
                .poll_token(TokenPollRequest {
                    token_url: self.endpoints.token_url.clone(),
                    client_id: self.endpoints.client_id.clone(),
                    device_code: started.device_code.clone(),
                    grant_type: DEVICE_GRANT_TYPE.to_string(),
                })
                .await?;

            if let Some(access_token) = response.access_token {
                return Ok(CopilotAccount {
                    user_code_hint: user_code_hint(&started.user_code),
                    access_token,
                });
            }
            match response.error.as_deref() {
                Some("authorization_pending") => {}
                Some("slow_down") => {
                    interval = adjust_interval(interval, "slow_down", self.slow_down_increment);
                }
                Some("expired_token") => {
                    return Err(SignInError::Vendor(
                        "the code expired; start again".to_string(),
                    ));
                }
                Some("access_denied") => {
                    return Err(SignInError::Vendor("the sign-in was declined".to_string()));
                }
                Some(other) => {
                    return Err(SignInError::Vendor(format!("device flow error: {other}")));
                }
                None => {
                    return Err(SignInError::Vendor(
                        "token endpoint returned neither a token nor an error".to_string(),
                    ));
                }
            }

            let sleep_for = interval.max(self.poll_floor);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(SignInError::Timeout);
            }
            let sleep_for = sleep_for.min(remaining);
            if sleep_for.is_zero() {
                // yield instead of a zero-duration sleep so accelerated
                // tests still give the fake layer a scheduling point.
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep(sleep_for).await;
            }
        }
    }
}

/// RFC 8628 §3.5 interval adjustment: `slow_down` grows the interval,
/// anything else leaves it untouched.
fn adjust_interval(current: Duration, error: &str, increment: Duration) -> Duration {
    if error == "slow_down" {
        current + increment
    } else {
        current
    }
}

/// Masked account label derived from the user code (`WDJB-MJHT` →
/// `wdjb****`): enough to recognize the account, not enough to reuse.
fn user_code_hint(user_code: &str) -> String {
    let lower = user_code.to_lowercase();
    match lower.char_indices().nth(4) {
        Some((idx, _)) => format!("{}****", &lower[..idx]),
        None => lower,
    }
}

// ---------------------------------------------------------------------------
// Encoding helpers (duplicated from gemini.rs, where they are private;
// hoisting them into a shared module is out of this module's scope)
// ---------------------------------------------------------------------------

/// Percent-encode for form bodies (unreserved set kept literal).
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

/// `application/x-www-form-urlencoded` body (keys are static and safe).
fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;

    // ------------------------------------------------------------------
    // Fake HTTP layer
    // ------------------------------------------------------------------

    /// Scriptable [`DeviceHttp`] fake. Cheap to clone; all clones share the
    /// same call counters, recorded requests and scripted results. Once the
    /// scripted poll queue is exhausted the last scripted poll result is
    /// repeated (tests assert exact poll counts, so over-polling is
    /// visible).
    #[derive(Clone)]
    struct FakeDeviceHttp {
        device_calls: Arc<AtomicUsize>,
        poll_calls: Arc<AtomicUsize>,
        last_device_request: Arc<Mutex<Option<DeviceCodeRequest>>>,
        last_poll_request: Arc<Mutex<Option<TokenPollRequest>>>,
        poll_instants: Arc<Mutex<Vec<Instant>>>,
        device_result: Arc<Mutex<Result<DeviceCodeResponse, String>>>,
        polls: Arc<Mutex<VecDeque<Result<TokenPollResponse, String>>>>,
        last_poll: Arc<Mutex<Option<Result<TokenPollResponse, String>>>>,
    }

    impl FakeDeviceHttp {
        fn new(
            device_result: Result<DeviceCodeResponse, String>,
            polls: Vec<Result<TokenPollResponse, String>>,
        ) -> Self {
            Self {
                device_calls: Arc::new(AtomicUsize::new(0)),
                poll_calls: Arc::new(AtomicUsize::new(0)),
                last_device_request: Arc::new(Mutex::new(None)),
                last_poll_request: Arc::new(Mutex::new(None)),
                poll_instants: Arc::new(Mutex::new(Vec::new())),
                device_result: Arc::new(Mutex::new(device_result)),
                polls: Arc::new(Mutex::new(polls.into_iter().collect())),
                last_poll: Arc::new(Mutex::new(None)),
            }
        }

        fn poll_calls(&self) -> usize {
            self.poll_calls.load(Ordering::SeqCst)
        }
    }

    impl DeviceHttp for FakeDeviceHttp {
        fn request_device_code(
            &self,
            request: DeviceCodeRequest,
        ) -> BoxFuture<'static, Result<DeviceCodeResponse, SignInError>> {
            self.device_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_device_request.lock().unwrap() = Some(request);
            let result = self.device_result.lock().unwrap().clone();
            Box::pin(async move { result.map_err(SignInError::Vendor) })
        }

        fn poll_token(
            &self,
            request: TokenPollRequest,
        ) -> BoxFuture<'static, Result<TokenPollResponse, SignInError>> {
            self.poll_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_poll_request.lock().unwrap() = Some(request);
            self.poll_instants.lock().unwrap().push(Instant::now());
            let result = {
                let mut queue = self.polls.lock().unwrap();
                match queue.pop_front() {
                    Some(result) => {
                        *self.last_poll.lock().unwrap() = Some(result.clone());
                        result
                    }
                    None => self
                        .last_poll
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| Err("poll script exhausted".to_string())),
                }
            };
            Box::pin(async move { result.map_err(SignInError::Vendor) })
        }
    }

    // ------------------------------------------------------------------
    // Fixtures
    // ------------------------------------------------------------------

    fn test_endpoints() -> CopilotEndpoints {
        CopilotEndpoints {
            device_url: "https://device.example.test/code".to_string(),
            token_url: "https://token.example.test/access_token".to_string(),
            verification_uri: "https://verify.example.test/device".to_string(),
            client_id: "test-client-id".to_string(),
        }
    }

    fn device_response(interval: Option<u64>) -> DeviceCodeResponse {
        DeviceCodeResponse {
            device_code: "device-code-1".to_string(),
            user_code: "WDJB-MJHT".to_string(),
            verification_uri: Some("https://verify.example.test/from-response".to_string()),
            interval,
        }
    }

    fn pending() -> Result<TokenPollResponse, String> {
        Ok(TokenPollResponse {
            access_token: None,
            error: Some("authorization_pending".to_string()),
        })
    }

    fn slow_down() -> Result<TokenPollResponse, String> {
        Ok(TokenPollResponse {
            access_token: None,
            error: Some("slow_down".to_string()),
        })
    }

    fn error_poll(error: &str) -> Result<TokenPollResponse, String> {
        Ok(TokenPollResponse {
            access_token: None,
            error: Some(error.to_string()),
        })
    }

    fn token(access_token: &str) -> Result<TokenPollResponse, String> {
        Ok(TokenPollResponse {
            access_token: Some(access_token.to_string()),
            error: None,
        })
    }

    /// A sign-in with a scripted HTTP layer, accelerated for tests
    /// (poll floor 0, device response advertises a 0s interval).
    fn accelerated_sign_in(
        device_result: Result<DeviceCodeResponse, String>,
        polls: Vec<Result<TokenPollResponse, String>>,
    ) -> (FakeDeviceHttp, CopilotSignIn) {
        let fake = FakeDeviceHttp::new(device_result, polls);
        let mut sign_in = CopilotSignIn::with_http(test_endpoints(), Arc::new(fake.clone()));
        sign_in.with_poll_floor(Duration::ZERO);
        (fake, sign_in)
    }

    // ------------------------------------------------------------------
    // Constants / defaults
    // ------------------------------------------------------------------

    #[test]
    fn defaults_match_github_copilot() {
        let endpoints = CopilotEndpoints::default();
        assert_eq!(endpoints.device_url, "https://github.com/login/device/code");
        assert_eq!(
            endpoints.token_url,
            "https://github.com/login/oauth/access_token"
        );
        assert_eq!(
            endpoints.verification_uri,
            "https://github.com/login/device"
        );
        assert_eq!(endpoints.client_id, "Iv1.b507a08c87ecfe98");
        assert_eq!(COPILOT_DEVICE_SCOPE, "read:user");
    }

    // ------------------------------------------------------------------
    // start()
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn start_parses_device_response() {
        let (fake, sign_in) = accelerated_sign_in(Ok(device_response(Some(7))), vec![]);

        let started = sign_in.start().await.unwrap();

        assert_eq!(started.device_code, "device-code-1");
        assert_eq!(started.user_code, "WDJB-MJHT");
        assert_eq!(
            started.verification_uri,
            "https://verify.example.test/from-response"
        );
        assert_eq!(started.interval, Duration::from_secs(7));

        let request = fake.last_device_request.lock().unwrap().clone().unwrap();
        assert_eq!(request.device_url, "https://device.example.test/code");
        assert_eq!(request.client_id, "test-client-id");
        assert_eq!(request.scope, "read:user");
    }

    #[tokio::test]
    async fn start_defaults_missing_interval_and_verification_uri() {
        let mut response = device_response(None);
        response.verification_uri = None;
        let (_, sign_in) = accelerated_sign_in(Ok(response), vec![]);

        let started = sign_in.start().await.unwrap();

        // RFC 8628 default interval is 5s; missing verification URI falls
        // back to the configured endpoint value.
        assert_eq!(started.interval, Duration::from_secs(5));
        assert_eq!(
            started.verification_uri,
            "https://verify.example.test/device"
        );
    }

    #[tokio::test]
    async fn start_failure_is_vendor_error() {
        let (_, sign_in) =
            accelerated_sign_in(Err("device endpoint returned 400".to_string()), vec![]);

        let err = sign_in.start().await.unwrap_err();
        assert!(matches!(err, SignInError::Vendor(ref m) if m.contains("400")));
    }

    // ------------------------------------------------------------------
    // wait(): happy path
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn wait_pending_pending_then_token() {
        let (fake, sign_in) = accelerated_sign_in(
            Ok(device_response(Some(0))),
            vec![pending(), pending(), token("gho_access_1")],
        );
        let started = sign_in.start().await.unwrap();

        let account = sign_in
            .wait(&started, Duration::from_secs(5))
            .await
            .unwrap();

        assert_eq!(account.access_token, "gho_access_1");
        assert_eq!(account.user_code_hint, "wdjb****");
        assert_eq!(fake.poll_calls(), 3, "pending, pending, then token");

        let request = fake.last_poll_request.lock().unwrap().clone().unwrap();
        assert_eq!(request.token_url, "https://token.example.test/access_token");
        assert_eq!(request.client_id, "test-client-id");
        assert_eq!(request.device_code, "device-code-1");
        assert_eq!(
            request.grant_type,
            "urn:ietf:params:oauth:grant-type:device_code"
        );
    }

    #[tokio::test]
    async fn wait_slow_down_grows_interval_then_succeeds() {
        // Slow-down increment shrunk to 300ms so the growth is observable
        // from the recorded poll timestamps without a 5s test. Script:
        // poll1 pending (gap ~0 with the advertised 0s interval), poll2
        // slow_down (interval grows to 300ms), poll3 pending (gap ≥ 300ms),
        // poll4 token.
        let (fake, mut sign_in) = accelerated_sign_in(
            Ok(device_response(Some(0))),
            vec![pending(), slow_down(), pending(), token("gho_access_2")],
        );
        sign_in.with_slow_down_increment(Duration::from_millis(300));
        let started = sign_in.start().await.unwrap();

        let account = sign_in
            .wait(&started, Duration::from_secs(5))
            .await
            .unwrap();

        assert_eq!(account.access_token, "gho_access_2");
        assert_eq!(fake.poll_calls(), 4);

        let instants = fake.poll_instants.lock().unwrap().clone();
        assert_eq!(instants.len(), 4);
        // Before the slow_down the scripted interval is 0 → the first gap
        // is negligible.
        assert!(instants[1] - instants[0] < Duration::from_millis(250));
        // After slow_down the poll interval grew by the configured
        // increment: both following gaps are at least ~300ms (small slack
        // for scheduler jitter) but far below the RFC default 5s.
        for gap in [instants[2] - instants[1], instants[3] - instants[2]] {
            assert!(
                gap >= Duration::from_millis(250),
                "gap after slow_down was only {gap:?}"
            );
            assert!(
                gap < Duration::from_secs(1),
                "gap {gap:?} should use the test increment, not the 5s default"
            );
        }
    }

    // ------------------------------------------------------------------
    // wait(): terminal errors
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn wait_expired_token_is_vendor_error() {
        let (_, sign_in) = accelerated_sign_in(
            Ok(device_response(Some(0))),
            vec![pending(), error_poll("expired_token")],
        );
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .wait(&started, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(
            matches!(err, SignInError::Vendor(ref m) if m.contains("expired")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn wait_access_denied_is_vendor_error() {
        let (_, sign_in) = accelerated_sign_in(
            Ok(device_response(Some(0))),
            vec![error_poll("access_denied")],
        );
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .wait(&started, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(
            matches!(err, SignInError::Vendor(ref m) if m.contains("declined")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn wait_unknown_flow_error_is_vendor_error() {
        let (_, sign_in) = accelerated_sign_in(
            Ok(device_response(Some(0))),
            vec![error_poll("server_error")],
        );
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .wait(&started, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(
            matches!(err, SignInError::Vendor(ref m) if m.contains("server_error")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn wait_response_without_token_or_error_is_vendor_error() {
        let (_, sign_in) = accelerated_sign_in(
            Ok(device_response(Some(0))),
            vec![Ok(TokenPollResponse {
                access_token: None,
                error: None,
            })],
        );
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .wait(&started, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(
            matches!(err, SignInError::Vendor(ref m) if m.contains("neither")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn wait_timeout_with_continuous_pending() {
        // The fake repeats `authorization_pending` forever; the wait must
        // give up at the deadline after polling more than once.
        let (fake, sign_in) = accelerated_sign_in(Ok(device_response(Some(0))), vec![pending()]);
        let started = sign_in.start().await.unwrap();

        let err = sign_in
            .wait(&started, Duration::from_millis(150))
            .await
            .unwrap_err();

        assert!(matches!(err, SignInError::Timeout));
        assert!(fake.poll_calls() >= 2, "should have polled repeatedly");
    }

    // ------------------------------------------------------------------
    // interval adjustment / hint masking / form encoding
    // ------------------------------------------------------------------

    #[test]
    fn adjust_interval_semantics() {
        let five = Duration::from_secs(5);
        assert_eq!(
            adjust_interval(Duration::from_secs(5), "slow_down", five),
            Duration::from_secs(10)
        );
        assert_eq!(
            adjust_interval(Duration::from_secs(0), "slow_down", five),
            five
        );
        assert_eq!(
            adjust_interval(Duration::from_secs(5), "authorization_pending", five),
            Duration::from_secs(5),
            "pending must not change the interval"
        );
    }

    #[test]
    fn user_code_hint_masks_after_four_chars() {
        assert_eq!(user_code_hint("WDJB-MJHT"), "wdjb****");
        assert_eq!(user_code_hint("abcd"), "abcd");
        assert_eq!(user_code_hint("AB"), "ab");
        assert_eq!(user_code_hint(""), "");
    }

    #[test]
    fn form_urlencoded_body() {
        let body = form_urlencoded(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", "Iv1.b507a08c87ecfe98"),
            ("scope", "read:user"),
        ]);
        assert_eq!(
            body,
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code\
&client_id=Iv1.b507a08c87ecfe98\
&scope=read%3Auser"
        );
    }

    // ------------------------------------------------------------------
    // Credential::OAuth storage round trip
    // ------------------------------------------------------------------

    #[test]
    fn oauth_credential_round_trips_with_kind_tag() {
        let cred = super::super::Credential::OAuth {
            provider: "copilot".to_string(),
            account: "wdjb****".to_string(),
            access_token: "gho_access_1".to_string(),
            refresh_token: None,
            expires_at_unix: Some(1_700_000_000),
        };

        let json = serde_json::to_value(&cred).unwrap();
        assert_eq!(json["kind"], "oauth");
        assert_eq!(json["provider"], "copilot");

        let back: super::super::Credential = serde_json::from_value(json).unwrap();
        assert_eq!(back, cred);
    }
}
