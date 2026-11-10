//! Browser page-session authentication: the page key, its cookie, and the
//! request policy that keeps cookie-authenticated management on the
//! loopback.
//!
//! The key is resolved once at startup (env, `<home>/web-key`, or a fresh
//! per-run key). A browser exchanges it for a `HttpOnly; SameSite=Strict`
//! cookie holding a random per-session token — never the key itself — whose
//! name carries the request's port; `require_management` accepts that
//! cookie only for loopback, same-origin requests.

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    Extension, Json,
    extract::rejection::JsonRejection,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::admin::AdminRuntime;
use crate::home::Home;
use crate::lockfile::InstanceLock;

/// Environment variable that pins the page key across restarts.
pub const WEB_KEY_ENV: &str = "ANY_CONVERTER_WEB_KEY";

/// The browser page-session credential.
///
/// Deliberately not `Debug`, `Serialize`, or `Clone`: a derived `Debug`
/// would leak the key. Share it behind `Arc` where sharing is needed.
pub struct WebAuth {
    key: String,
    persistent: bool,
    /// Live page-session tokens. The cookie never carries the key itself:
    /// a disclosed cookie is one revocable session, not the master key.
    /// In-memory on purpose — a restart asks for the key again — so a
    /// stolen token dies with the process that minted it.
    sessions: Mutex<HashSet<String>>,
    /// Fixed-window throttle on failed key exchanges.
    attempts: Mutex<AttemptWindow>,
}

/// Cap on live session tokens; pathological only, but the set must not
/// grow without bound. Clearing all sessions is the lazy eviction: every
/// holder re-enters the key once.
const SESSION_CAP: usize = 64;
/// How long a burst of failed key exchanges is remembered.
const ATTEMPT_WINDOW: Duration = Duration::from_secs(60);
/// Failed key exchanges allowed per window before the exchange throttles.
const ATTEMPT_LIMIT: u32 = 10;

#[derive(Default)]
struct AttemptWindow {
    started: Option<Instant>,
    failures: u32,
}

impl AttemptWindow {
    /// Whether one more attempt is allowed, first resetting an expired
    /// window.
    fn allows(&mut self) -> bool {
        let now = Instant::now();
        match self.started {
            Some(started) if now.duration_since(started) >= ATTEMPT_WINDOW => {
                self.started = Some(now);
                self.failures = 0;
                true
            }
            Some(_) => self.failures < ATTEMPT_LIMIT,
            None => {
                self.started = Some(now);
                true
            }
        }
    }

    fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    fn reset(&mut self) {
        self.started = None;
        self.failures = 0;
    }
}

/// Result of checking a page key at either exchange endpoint.
pub(crate) enum KeyExchange {
    Valid,
    Invalid,
    Throttled,
}

/// Failure to resolve a usable page key. Messages never include the key.
#[derive(Debug, Error)]
pub enum WebKeyError {
    #[error(
        "{WEB_KEY_ENV} is not a valid page key; a page key needs at least 16 characters from [A-Za-z0-9._~-]"
    )]
    InvalidEnvKey,
    #[error(
        "the page key file `{path}` does not hold a valid page key; a page key needs at least 16 characters from [A-Za-z0-9._~-]"
    )]
    InvalidFile { path: PathBuf },
    #[error("cannot read the page key file `{path}`: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write the page key file `{path}`: {source}")]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl WebAuth {
    /// Build a web auth from a known key. The key must be at least 16
    /// characters drawn from `[A-Za-z0-9._~-]` so it survives cookie
    /// serialization; anything else is [`WebKeyError::InvalidEnvKey`].
    pub fn from_key(key: impl Into<String>, persistent: bool) -> Result<Self, WebKeyError> {
        let key = key.into();
        if !valid_key(&key) {
            return Err(WebKeyError::InvalidEnvKey);
        }
        Ok(Self::with_key(key, persistent))
    }

    /// A fresh random key for this run, never written anywhere.
    pub fn ephemeral() -> Self {
        Self::with_key(uuid::Uuid::new_v4().simple().to_string(), false)
    }

    fn with_key(key: String, persistent: bool) -> Self {
        Self {
            key,
            persistent,
            sessions: Mutex::new(HashSet::new()),
            attempts: Mutex::new(AttemptWindow::default()),
        }
    }

    /// Constant-time comparison of a provided key against this one.
    pub fn verify(&self, provided: &str) -> bool {
        self.key.as_bytes().ct_eq(provided.as_bytes()).into()
    }

    /// Whether the key is pinned across restarts (env or file) rather than
    /// per-run.
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Mint a fresh session token. This is the cookie value from here on:
    /// random, revocable, and worthless after a restart.
    pub(crate) fn mint_session(&self) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let mut sessions = lock(&self.sessions);
        if sessions.len() >= SESSION_CAP {
            sessions.clear();
        }
        sessions.insert(token.clone());
        token
    }

    /// Whether `token` is a live session. The tokens are random 122-bit
    /// values, so the set lookup needs no constant-time comparison.
    pub(crate) fn verify_session(&self, token: &str) -> bool {
        lock(&self.sessions).contains(token)
    }

    /// Revoke one session (sign-out).
    pub(crate) fn drop_session(&self, token: &str) {
        lock(&self.sessions).remove(token);
    }

    /// Check a page key and account for failed attempts. A valid key always
    /// succeeds — bad guesses must not let a LAN client denial-of-service the
    /// operator — and resets the failure window. Invalid keys stop doing the
    /// expensive comparison after the fixed-window limit.
    pub(crate) fn exchange_key(&self, provided: &str) -> KeyExchange {
        let mut attempts = lock(&self.attempts);
        if self.verify(provided) {
            attempts.reset();
            return KeyExchange::Valid;
        }
        if !attempts.allows() {
            return KeyExchange::Throttled;
        }
        attempts.record_failure();
        KeyExchange::Invalid
    }

    #[cfg(test)]
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// The one-time browser entry URL: `http://<host>:<port>/?k=<key>`.
    /// Wildcard bind addresses print `127.0.0.1`; IPv6 addresses are
    /// bracketed.
    pub fn url(&self, bound: std::net::SocketAddr) -> String {
        self.url_for(&bound.ip().to_string(), bound.port())
    }

    /// `http://<host>:<port>/` with no key, for log lines that must not
    /// carry a persistent secret.
    pub fn base_url(&self, bound: std::net::SocketAddr) -> String {
        format!(
            "http://{}:{}/",
            display_host_str(&bound.ip().to_string()),
            bound.port()
        )
    }

    /// `http://<host>:<port>/?k=<key>`; wildcard hosts (`0.0.0.0`, `::`)
    /// become `127.0.0.1`, IPv6 is bracketed, `localhost` stays as given.
    pub fn url_for(&self, host: &str, port: u16) -> String {
        format!("http://{}:{}/?k={}", display_host_str(host), port, self.key)
    }

    /// The `Set-Cookie` value for the request's host port. The cookie name
    /// carries the port because browsers scope cookies per host only.
    pub(crate) fn session_cookie(&self, headers: &HeaderMap) -> String {
        set_cookie_value(self, host_port(headers))
    }

    /// The `Set-Cookie` value that clears the session cookie.
    pub(crate) fn clear_cookie(headers: &HeaderMap) -> String {
        format!(
            "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
            session_cookie_name(headers)
        )
    }
}

/// Poison-tolerant lock helper: a panicking holder never leaves the auth
/// unusable for the rest of the process.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `Set-Cookie` value for a page session on `port` (the browser's view
/// of the request, from its `Host` header). The single cookie-string
/// builder, shared by the `/v1/session` exchange and the `/?k=` link.
pub(crate) fn set_cookie_value(web_auth: &WebAuth, port: u16) -> String {
    let mut cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Priority=High",
        session_cookie_name_for_port(port),
        // The session token, never the key: a disclosed cookie is one
        // revocable session, not the master key.
        web_auth.mint_session()
    );
    if web_auth.is_persistent() {
        cookie.push_str("; Max-Age=2592000");
    }
    cookie
}

/// Resolve the page key at startup: `ANY_CONVERTER_WEB_KEY`, then
/// `<home>/web-key`, then a fresh per-run key.
pub fn resolve(home: &Home) -> Result<WebAuth, WebKeyError> {
    resolve_with_env(home, std::env::var(WEB_KEY_ENV).ok())
}

/// [`resolve`] with the environment lookup injected for hermetic tests.
pub(crate) fn resolve_with_env(
    home: &Home,
    env_key: Option<String>,
) -> Result<WebAuth, WebKeyError> {
    if let Some(key) = env_key {
        return WebAuth::from_key(key, true);
    }
    read_key_file(home).map(|auth| auth.unwrap_or_else(WebAuth::ephemeral))
}

/// Read `<home>/web-key` only; ignore `ANY_CONVERTER_WEB_KEY`.
/// `Ok(None)` when the file is absent.
pub fn read_key_file(home: &Home) -> Result<Option<WebAuth>, WebKeyError> {
    let path = home.web_key_path();
    match std::fs::read_to_string(&path) {
        Ok(source) => {
            let key = source.trim_end_matches(['\r', '\n']).to_owned();
            WebAuth::from_key(key, true)
                .map(Some)
                .map_err(|error| match error {
                    WebKeyError::InvalidEnvKey => WebKeyError::InvalidFile { path },
                    other => other,
                })
        }
        // No file means no pinned key, not an error.
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(WebKeyError::ReadFile { path, source }),
    }
}

/// Create `<home>/web-key` (0600, 32 hex chars) when absent, or validate
/// the existing file. Repairs the mode to 0600 on Unix. Idempotent.
///
/// A kernel-backed lock serializes the first publication. The write still
/// goes through a unique temp file, so a crash cannot leave a partial key and
/// no successful caller can observe a key that a competitor later replaces.
pub fn ensure_key_file(home: &Home) -> Result<(), WebKeyError> {
    std::fs::create_dir_all(home.root()).map_err(|source| WebKeyError::WriteFile {
        path: home.root().to_path_buf(),
        source,
    })?;
    let lock_path = home.web_key_lock_path();
    let _guard = InstanceLock::new(&lock_path)
        .acquire_timeout(Duration::from_secs(10))
        .map_err(|source| WebKeyError::WriteFile {
            path: lock_path,
            source,
        })?;
    let path = home.web_key_path();
    if path.exists() {
        read_key_file(home)?;
        repair_mode(&path)?;
        return Ok(());
    }
    let key = uuid::Uuid::new_v4().simple().to_string();
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = options
        .open(&temp)
        .and_then(|mut file| file.write_all(key.as_bytes()))
        .and_then(|()| std::fs::rename(&temp, &path));
    if let Err(source) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(WebKeyError::WriteFile { path, source });
    }
    repair_mode(&path)
}

#[cfg(unix)]
fn repair_mode(path: &std::path::Path) -> Result<(), WebKeyError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        WebKeyError::WriteFile {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn repair_mode(_path: &std::path::Path) -> Result<(), WebKeyError> {
    Ok(())
}

fn valid_key(key: &str) -> bool {
    key.len() >= 16
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-'))
}

/// The port the browser addressed, from the `Host` header; `80` when the
/// header is absent or has no parsable port.
pub(crate) fn host_port(headers: &HeaderMap) -> u16 {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return 80;
    };
    let host = host.trim();
    if let Some((_, rest)) = host.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
        return rest
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(80);
    }
    host.rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .unwrap_or(80)
}

/// The session cookie name. Browsers scope cookies per host and ignore the
/// port, so the name carries it: two gateways on sibling loopback ports
/// must not overwrite each other's session.
fn session_cookie_name(headers: &HeaderMap) -> String {
    session_cookie_name_for_port(host_port(headers))
}

fn session_cookie_name_for_port(port: u16) -> String {
    format!("any-converter-session-{}", port)
}

/// Whether the `Host` header is a loopback address (`127.0.0.1`,
/// `localhost`, or `[::1]`, each with an optional port), case
/// insensitively. Cookie auth is loopback-only by construction.
fn is_loopback_host(headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let host = host.trim();
    if let Some((address, rest)) = host.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
        let port_ok = rest.is_empty()
            || rest.strip_prefix(':').is_some_and(|port| {
                !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
            });
        return port_ok && address.eq_ignore_ascii_case("::1");
    }
    match host.rsplit_once(':') {
        Some((name, port))
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            name.eq_ignore_ascii_case("127.0.0.1") || name.eq_ignore_ascii_case("localhost")
        }
        _ => host.eq_ignore_ascii_case("127.0.0.1") || host.eq_ignore_ascii_case("localhost"),
    }
}

/// Whether the request's `Sec-Fetch-Site` metadata marks it as first-party:
/// absent, `none`, or `same-origin`.
fn fetch_site_allowed(headers: &HeaderMap) -> bool {
    let Some(site) = headers.get("sec-fetch-site") else {
        return true;
    };
    site.to_str().is_ok_and(|site| {
        site.eq_ignore_ascii_case("none") || site.eq_ignore_ascii_case("same-origin")
    })
}

/// Whether the request's `Origin`, when present, is the request host.
fn origin_matches(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(origin) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    origin
        .authority()
        .is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(host))
}

/// Outcome of checking a request against the page session policy.
pub(crate) enum SessionCheck {
    Valid,
    Missing,
    Invalid,
    Forbidden,
}

/// Whether a request satisfies the page-session policy: a loopback `Host`,
/// first-party `Sec-Fetch-Site` metadata, and, on mutations, a matching
/// `Origin`. Shared by the cookie check and the `/?k=` exchange.
pub(crate) fn policy_allows(headers: &HeaderMap, method: &Method) -> bool {
    if !is_loopback_host(headers) || !fetch_site_allowed(headers) {
        return false;
    }
    let is_read = method == Method::GET || method == Method::HEAD || method == Method::OPTIONS;
    is_read || origin_matches(headers)
}

/// Check a request's page session: the loopback / `Sec-Fetch-Site` /
/// `Origin` policy first, then the port-scoped cookie.
pub(crate) fn check_session(
    headers: &HeaderMap,
    method: &Method,
    web_auth: &WebAuth,
) -> SessionCheck {
    if !policy_allows(headers, method) {
        return SessionCheck::Forbidden;
    }
    let Some(value) = cookie_value(headers, &session_cookie_name(headers)) else {
        return SessionCheck::Missing;
    };
    if web_auth.verify_session(&value) {
        SessionCheck::Valid
    } else {
        SessionCheck::Invalid
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (cookie_name, value) = part.trim().split_once('=')?;
        (cookie_name.trim() == name).then(|| value.trim().to_owned())
    })
}

fn display_host(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        std::net::IpAddr::V4(ip) => ip.to_string(),
        std::net::IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

/// The display host for `url_for`: wildcards become `127.0.0.1`, bare
/// IPv6 addresses are bracketed, bracketed IPv6 and hostnames stay as
/// given.
fn display_host_str(host: &str) -> String {
    let bare = host
        .strip_prefix('[')
        .and_then(|bracketed| bracketed.strip_suffix(']'))
        .unwrap_or(host);
    match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => display_host(ip),
        Err(_) => host.to_owned(),
    }
}

/// `GET /v1/session`: report whether the request holds a valid page
/// session.
pub(crate) async fn get_session(
    Extension(runtime): Extension<AdminRuntime>,
    headers: HeaderMap,
) -> Response {
    let Some(web_auth) = runtime.web_auth.as_deref() else {
        return session_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "page sessions are not configured",
        );
    };
    match check_session(&headers, &Method::GET, web_auth) {
        SessionCheck::Valid => no_store(StatusCode::NO_CONTENT.into_response()),
        SessionCheck::Forbidden => {
            session_error(StatusCode::FORBIDDEN, "page sessions are loopback-only")
        }
        SessionCheck::Missing | SessionCheck::Invalid => {
            session_error(StatusCode::UNAUTHORIZED, "no valid page session")
        }
    }
}

/// `POST /v1/session`: exchange the page key in the body for the session
/// cookie.
pub(crate) async fn post_session(
    Extension(runtime): Extension<AdminRuntime>,
    headers: HeaderMap,
    body: Result<Json<SessionKeyBody>, JsonRejection>,
) -> Response {
    let Some(web_auth) = runtime.web_auth.as_deref() else {
        return session_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "page sessions are not configured",
        );
    };
    if !policy_allows(&headers, &Method::POST) {
        return session_error(StatusCode::FORBIDDEN, "page sessions are loopback-only");
    }
    let Ok(Json(body)) = body else {
        return session_error(StatusCode::BAD_REQUEST, "invalid request body");
    };
    match web_auth.exchange_key(&body.key) {
        KeyExchange::Valid => {
            with_cookie(StatusCode::NO_CONTENT, &web_auth.session_cookie(&headers))
        }
        KeyExchange::Invalid => session_error(StatusCode::UNAUTHORIZED, "invalid web key"),
        KeyExchange::Throttled => session_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts; wait a minute and try again",
        ),
    }
}

/// `DELETE /v1/session`: revoke the session and clear the cookie. Always
/// succeeds so the sign-out button works even after a restart changed the
/// key.
pub(crate) async fn delete_session(
    Extension(runtime): Extension<AdminRuntime>,
    headers: HeaderMap,
) -> Response {
    let Some(web_auth) = runtime.web_auth.as_deref() else {
        return no_store(StatusCode::NO_CONTENT.into_response());
    };
    if let Some(token) = cookie_value(&headers, &session_cookie_name(&headers)) {
        web_auth.drop_session(&token);
    }
    with_cookie(StatusCode::NO_CONTENT, &WebAuth::clear_cookie(&headers))
}

#[derive(Deserialize)]
pub(crate) struct SessionKeyBody {
    key: String,
}

fn session_error(status: StatusCode, message: &str) -> Response {
    no_store((status, Json(json!({ "error": message }))).into_response())
}

/// Session responses must never be cached: they can carry a session token
/// or an auth challenge.
fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn with_cookie(status: StatusCode, cookie: &str) -> Response {
    let mut response = no_store(status.into_response());
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use axum::http::HeaderName;
    use std::net::SocketAddr;

    const KEY: &str = "abcdefghijklmnop";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    fn with_cookie(base: &HeaderMap, cookie: &str) -> HeaderMap {
        let mut headers = base.clone();
        headers.insert(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
        headers
    }

    #[test]
    fn key_validation_accepts_only_sixteen_safe_characters() {
        assert!(WebAuth::from_key("123456789012345", false).is_err());
        for bad in [
            "a space 0123456789",
            "semi;colon0123456",
            "quote\"0123456789",
            "comma,01234567890",
        ] {
            assert!(WebAuth::from_key(bad, false).is_err(), "{bad:?}");
        }
        let auth = WebAuth::from_key(KEY, false).unwrap();
        assert!(auth.verify(KEY));
        assert!(!auth.verify("abcdefghijklmnoq"));
    }

    #[test]
    fn resolve_is_ephemeral_without_env_or_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        let auth = resolve_with_env(&home, None).unwrap();

        assert!(!auth.is_persistent());
        assert_eq!(auth.key().len(), 32);
        assert!(auth.verify(auth.key()));
    }

    #[test]
    fn resolve_reads_and_trims_the_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        std::fs::write(home.web_key_path(), format!("{KEY}\n")).unwrap();

        let auth = resolve_with_env(&home, None).unwrap();
        assert!(auth.is_persistent());
        assert!(auth.verify(KEY));
    }

    #[test]
    fn resolve_rejects_a_short_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        std::fs::write(home.web_key_path(), "short\n").unwrap();

        let error = resolve_with_env(&home, None).err().unwrap();
        match error {
            WebKeyError::InvalidFile { path } => assert_eq!(path, home.web_key_path()),
            other => panic!("expected InvalidFile, got {other:?}"),
        }
    }

    #[test]
    fn resolve_rejects_an_invalid_env_key() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        let error = resolve_with_env(&home, Some("short".to_owned()))
            .err()
            .unwrap();
        assert!(matches!(error, WebKeyError::InvalidEnvKey));
    }

    #[test]
    fn resolve_prefers_the_env_key_over_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        std::fs::write(home.web_key_path(), "file-key-0123456789\n").unwrap();

        let auth = resolve_with_env(&home, Some("env-key-0123456789".to_owned())).unwrap();
        assert!(auth.is_persistent());
        assert!(auth.verify("env-key-0123456789"));
        assert!(!auth.verify("file-key-0123456789"));
    }

    #[test]
    fn resolve_reports_unreadable_key_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        std::fs::create_dir_all(home.web_key_path()).unwrap();

        let error = resolve_with_env(&home, None).err().unwrap();
        assert!(matches!(error, WebKeyError::ReadFile { .. }));
    }

    #[test]
    fn host_port_reads_the_host_header() {
        for (host, port) in [
            ("127.0.0.1:1228", 1228),
            ("localhost", 80),
            ("[::1]:1229", 1229),
            ("[::1]", 80),
        ] {
            assert_eq!(host_port(&headers(&[("host", host)])), port, "{host}");
        }
        assert_eq!(host_port(&HeaderMap::new()), 80);
    }

    #[test]
    fn session_cookie_name_carries_the_host_port() {
        assert_eq!(
            session_cookie_name(&headers(&[("host", "127.0.0.1:1228")])),
            "any-converter-session-1228"
        );
        assert_eq!(
            session_cookie_name(&HeaderMap::new()),
            "any-converter-session-80"
        );
    }

    #[test]
    fn loopback_hosts_are_recognized() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:1",
            "localhost:2",
            "[::1]:3",
            "LOCALHOST",
        ] {
            assert!(is_loopback_host(&headers(&[("host", host)])), "{host}");
        }
        for host in [
            "example.com",
            "192.168.1.5:1228",
            "127.0.0.1.evil.com",
            "localhost.evil:1228",
        ] {
            assert!(!is_loopback_host(&headers(&[("host", host)])), "{host}");
        }
        assert!(!is_loopback_host(&HeaderMap::new()));
    }

    #[test]
    fn fetch_site_policy_accepts_only_first_party_requests() {
        assert!(fetch_site_allowed(&HeaderMap::new()));
        for site in ["none", "same-origin"] {
            assert!(
                fetch_site_allowed(&headers(&[("sec-fetch-site", site)])),
                "{site}"
            );
        }
        for site in ["same-site", "cross-site", "nonsense"] {
            assert!(
                !fetch_site_allowed(&headers(&[("sec-fetch-site", site)])),
                "{site}"
            );
        }
    }

    #[test]
    fn origin_must_match_the_host_header() {
        assert!(origin_matches(&HeaderMap::new()));
        assert!(origin_matches(&headers(&[
            ("origin", "http://127.0.0.1:1228"),
            ("host", "127.0.0.1:1228")
        ])));
        for origin in [
            "http://127.0.0.1:1229",
            "http://evil.example",
            "null",
            "not a uri",
        ] {
            assert!(
                !origin_matches(&headers(&[("origin", origin), ("host", "127.0.0.1:1228")])),
                "{origin}"
            );
        }
    }

    #[test]
    fn check_session_reads_the_port_scoped_cookie() {
        let auth = WebAuth::from_key(KEY, true).unwrap();
        let loopback = headers(&[("host", "127.0.0.1:1228")]);
        assert!(matches!(
            check_session(&loopback, &Method::GET, &auth),
            SessionCheck::Missing
        ));

        let wrong = with_cookie(&loopback, "any-converter-session-1228=wrong-value-1234");
        assert!(matches!(
            check_session(&wrong, &Method::GET, &auth),
            SessionCheck::Invalid
        ));

        // A live minted token — never the key itself — is the session.
        let token = auth.mint_session();
        let right = with_cookie(&loopback, &format!("any-converter-session-1228={token}"));
        assert!(matches!(
            check_session(&right, &Method::GET, &auth),
            SessionCheck::Valid
        ));

        // Unrelated cookies are ignored; only the exact name counts.
        let mixed = with_cookie(
            &loopback,
            &format!("foo=bar; any-converter-session-1228={token}; other=1"),
        );
        assert!(matches!(
            check_session(&mixed, &Method::GET, &auth),
            SessionCheck::Valid
        ));
    }

    #[test]
    fn check_session_enforces_the_loopback_and_fetch_policy() {
        let auth = WebAuth::from_key(KEY, true).unwrap();
        let cookie = "any-converter-session-1228=abcdefghijklmnop";

        let cross_site = headers(&[
            ("host", "127.0.0.1:1228"),
            ("cookie", cookie),
            ("sec-fetch-site", "cross-site"),
        ]);
        assert!(matches!(
            check_session(&cross_site, &Method::GET, &auth),
            SessionCheck::Forbidden
        ));

        let non_loopback = headers(&[("host", "192.168.1.5:1228"), ("cookie", cookie)]);
        assert!(matches!(
            check_session(&non_loopback, &Method::GET, &auth),
            SessionCheck::Forbidden
        ));
    }

    #[test]
    fn check_session_checks_the_origin_only_for_mutations() {
        let auth = WebAuth::from_key(KEY, true).unwrap();
        let mismatched = headers(&[
            ("host", "127.0.0.1:1228"),
            (
                "cookie",
                &format!("any-converter-session-1228={}", auth.mint_session()),
            ),
            ("origin", "http://evil.example"),
        ]);
        assert!(matches!(
            check_session(&mismatched, &Method::POST, &auth),
            SessionCheck::Forbidden
        ));
        assert!(matches!(
            check_session(&mismatched, &Method::GET, &auth),
            SessionCheck::Valid
        ));
    }

    #[test]
    fn persistent_sessions_get_a_long_lived_cookie() {
        let auth = WebAuth::from_key(KEY, true).unwrap();
        let cookie = auth.session_cookie(&headers(&[("host", "127.0.0.1:1228")]));
        assert!(cookie.starts_with("any-converter-session-1228="));
        // The value is a minted token, never the key.
        assert!(!cookie.contains(KEY));
        for part in [
            "Path=/",
            "HttpOnly",
            "SameSite=Strict",
            "Priority=High",
            "Max-Age=2592000",
        ] {
            assert!(cookie.contains(part), "{cookie}");
        }
    }

    #[test]
    fn ephemeral_sessions_get_a_session_cookie() {
        let auth = WebAuth::ephemeral();
        let cookie = auth.session_cookie(&headers(&[("host", "127.0.0.1:1228")]));
        assert!(cookie.starts_with("any-converter-session-1228="));
        assert!(!cookie.contains("Max-Age"));
        for part in ["Path=/", "HttpOnly", "SameSite=Strict", "Priority=High"] {
            assert!(cookie.contains(part), "{cookie}");
        }
    }

    #[test]
    fn clear_cookie_expires_the_port_scoped_cookie() {
        let cookie = WebAuth::clear_cookie(&headers(&[("host", "127.0.0.1:1228")]));
        assert!(cookie.starts_with("any-converter-session-1228=;"));
        assert!(cookie.contains("Max-Age=0"));
    }

    #[test]
    fn url_prints_loopback_for_wildcard_binds_and_brackets_ipv6() {
        let auth = WebAuth::from_key(KEY, true).unwrap();
        assert_eq!(
            auth.url("0.0.0.0:1228".parse::<SocketAddr>().unwrap()),
            "http://127.0.0.1:1228/?k=abcdefghijklmnop"
        );
        assert_eq!(
            auth.url("[::]:1228".parse::<SocketAddr>().unwrap()),
            "http://127.0.0.1:1228/?k=abcdefghijklmnop"
        );
        assert_eq!(
            auth.url("127.0.0.1:1228".parse::<SocketAddr>().unwrap()),
            "http://127.0.0.1:1228/?k=abcdefghijklmnop"
        );
        assert_eq!(
            auth.url("[::1]:1228".parse::<SocketAddr>().unwrap()),
            "http://[::1]:1228/?k=abcdefghijklmnop"
        );
    }

    #[test]
    fn url_for_maps_wildcards_and_brackets_bare_ipv6() {
        let auth = WebAuth::from_key(KEY, true).unwrap();
        assert_eq!(
            auth.url_for("0.0.0.0", 1228),
            "http://127.0.0.1:1228/?k=abcdefghijklmnop"
        );
        assert_eq!(
            auth.url_for("::", 1228),
            "http://127.0.0.1:1228/?k=abcdefghijklmnop"
        );
        assert_eq!(
            auth.url_for("[::]", 1228),
            "http://127.0.0.1:1228/?k=abcdefghijklmnop"
        );
        assert_eq!(
            auth.url_for("::1", 1228),
            "http://[::1]:1228/?k=abcdefghijklmnop"
        );
        assert_eq!(
            auth.url_for("[::1]", 1228),
            "http://[::1]:1228/?k=abcdefghijklmnop"
        );
        assert_eq!(
            auth.url_for("localhost", 80),
            "http://localhost:80/?k=abcdefghijklmnop"
        );
    }

    #[test]
    fn read_key_file_is_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        assert!(read_key_file(&home).unwrap().is_none());
    }

    #[test]
    fn ensure_key_file_creates_a_valid_key() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        ensure_key_file(&home).unwrap();

        let auth = read_key_file(&home).unwrap().unwrap();
        assert!(auth.is_persistent());
        assert_eq!(auth.key().len(), 32);
        assert!(auth.key().bytes().all(|byte| byte.is_ascii_hexdigit()));
        let source = std::fs::read_to_string(home.web_key_path()).unwrap();
        assert!(
            !source.ends_with('\n'),
            "the key file has no trailing newline"
        );
        assert_eq!(source.len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(home.web_key_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn ensure_key_file_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        ensure_key_file(&home).unwrap();
        let first = std::fs::read_to_string(home.web_key_path()).unwrap();
        ensure_key_file(&home).unwrap();
        assert_eq!(std::fs::read_to_string(home.web_key_path()).unwrap(), first);
    }

    #[test]
    fn concurrent_key_creation_publishes_one_stable_key() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        const WORKERS: usize = 16;
        let barrier = std::sync::Barrier::new(WORKERS + 1);
        let observed = std::sync::Mutex::new(Vec::new());

        std::thread::scope(|scope| {
            for _ in 0..WORKERS {
                let home = home.clone();
                let barrier = &barrier;
                let observed = &observed;
                scope.spawn(move || {
                    barrier.wait();
                    ensure_key_file(&home).unwrap();
                    observed
                        .lock()
                        .unwrap()
                        .push(std::fs::read_to_string(home.web_key_path()).unwrap());
                });
            }
            barrier.wait();
        });

        let key = std::fs::read_to_string(home.web_key_path()).unwrap();
        assert_eq!(key.len(), 32);
        assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(
            observed.lock().unwrap().iter().all(|value| value == &key),
            "a caller observed a key that was later replaced"
        );
        let leftovers = std::fs::read_dir(home.root())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("web-key.tmp-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_key_file_repairs_the_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        std::fs::write(home.web_key_path(), "a-file-key-0123456789").unwrap();
        std::fs::set_permissions(home.web_key_path(), std::fs::Permissions::from_mode(0o644))
            .unwrap();

        ensure_key_file(&home).unwrap();
        let mode = std::fs::metadata(home.web_key_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn ensure_key_file_rejects_an_invalid_existing_file_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        std::fs::create_dir_all(home.root()).unwrap();
        std::fs::write(home.web_key_path(), "short").unwrap();

        let error = ensure_key_file(&home).unwrap_err();
        assert!(
            matches!(error, WebKeyError::InvalidFile { .. }),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(home.web_key_path()).unwrap(),
            "short"
        );
    }
}
