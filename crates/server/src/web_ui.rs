//! UI serving: the embedded or directory-backed SPA, the `/?k=` page-key
//! exchange, and the SPA fallback for non-API paths.
//!
//! The UI is embedded at compile time when `apps/desktop/dist/index.html`
//! exists (`has_ui_dist`), loaded from [`WEB_DIR_ENV`] otherwise, or
//! unavailable (a 503 with build instructions). The page key never appears
//! in response bodies or headers; the session cookie holds a random
//! revocable token, never the key. A persistent key is never logged (the
//! startup line prints the base URL only); an ephemeral run's one-time
//! unlock URL is logged once, and log files are 0600.

use std::borrow::Cow;
use std::path::{Component, PathBuf};

use axum::{
    Extension,
    body::Body,
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};

use crate::admin::AdminRuntime;
use crate::web_auth::{KeyExchange, WebAuth, host_port, policy_allows, set_cookie_value};

/// Environment variable naming a directory holding the built UI
/// (`index.html` plus `assets/`), served instead of the embedded copy.
pub const WEB_DIR_ENV: &str = "ANY_CONVERTER_WEB_DIR";

/// Served for `GET /` and the SPA fallback when no UI is available.
pub(crate) const UI_NOT_BUILT: &str = "UI assets are not built.\nRun: pnpm --filter @any-converter/desktop build && cargo build -p any-converter --release\nor set ANY_CONVERTER_WEB_DIR to a directory containing index.html.";

/// `Cache-Control` for the hashed `/assets/*` files.
const ASSETS_CACHE: &str = "public, max-age=31536000, immutable";

/// Content-Security-Policy for `index.html`.
const INDEX_CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'";

/// The prebuilt SPA embedded in the server binary at compile time.
#[cfg(has_ui_dist)]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../apps/desktop/dist/"]
struct Assets;

/// Where the UI comes from.
#[derive(Clone)]
pub enum WebUi {
    /// The SPA embedded in the server binary at compile time.
    #[cfg(has_ui_dist)]
    Embedded,
    /// A canonicalized directory serving `index.html` and friends.
    Dir(PathBuf),
    /// No UI: `/` answers 503 with build instructions.
    Disabled,
}

impl WebUi {
    /// Resolve the UI source at startup: the embedded build, then
    /// [`WEB_DIR_ENV`], then nothing.
    pub fn discover() -> WebUi {
        #[cfg(has_ui_dist)]
        {
            WebUi::Embedded
        }
        #[cfg(not(has_ui_dist))]
        {
            match std::env::var_os(WEB_DIR_ENV) {
                Some(root) => WebUi::from_dir(root),
                None => {
                    log::warn!(
                        "UI assets are not built; GET / answers 503. \
                         Run: pnpm --filter @any-converter/desktop build && cargo build -p any-converter --release, \
                         or set {WEB_DIR_ENV} to a directory containing index.html."
                    );
                    WebUi::Disabled
                }
            }
        }
    }

    /// Serve the UI from a directory. The root is canonicalized once; a
    /// root that cannot be canonicalized degrades to [`WebUi::Disabled`].
    pub fn from_dir(root: impl Into<PathBuf>) -> WebUi {
        let root = root.into();
        match std::fs::canonicalize(&root) {
            Ok(canonical) => WebUi::Dir(canonical),
            Err(error) => {
                log::warn!(
                    "web UI directory `{}` is not usable: {error}",
                    root.display()
                );
                WebUi::Disabled
            }
        }
    }

    /// The bytes of `path` (no leading `/`), or `None` when the file does
    /// not exist. Directory roots reject `..`, absolute, or symlink
    /// escapes.
    pub fn file(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        match self {
            #[cfg(has_ui_dist)]
            WebUi::Embedded => Assets::get(path).map(|file| file.data),
            WebUi::Dir(root) => dir_file(root, path),
            WebUi::Disabled => None,
        }
    }

    /// `file("index.html")`.
    pub fn index(&self) -> Option<Cow<'static, [u8]>> {
        self.file("index.html")
    }
}

/// Resolve `path` under the canonical `root`, never leaving it. Rejects
/// `..`, rooted, and empty components outright; symlinks resolve before the
/// prefix check so an inside link to an outside file is refused too.
fn dir_file(root: &std::path::Path, path: &str) -> Option<Cow<'static, [u8]>> {
    let relative: PathBuf = path.split('/').collect();
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let target = std::fs::canonicalize(root.join(relative)).ok()?;
    if !target.starts_with(root) {
        return None;
    }
    std::fs::read(target).ok().map(Cow::Owned)
}

/// The `Content-Type` for a file path, by extension.
pub(crate) fn content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("json") | Some("map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// `GET /`: exchange `?k=` for the session cookie, otherwise the SPA.
///
/// The URI is never logged here: the query may carry the page key.
pub(crate) async fn handle_root(
    Extension(runtime): Extension<AdminRuntime>,
    Extension(ui): Extension<WebUi>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if let Some((key, remaining)) = page_key_from_query(uri.query()) {
        let Some(web_auth) = runtime.web_auth.as_deref() else {
            return text(
                StatusCode::SERVICE_UNAVAILABLE,
                "page sessions are not configured",
            );
        };
        if !policy_allows(&headers, &Method::GET) {
            return text(StatusCode::FORBIDDEN, "page sessions are loopback-only");
        }
        match web_auth.exchange_key(&key) {
            KeyExchange::Valid => {
                return redirect_after_key_exchange(uri.path(), &remaining, web_auth, &headers);
            }
            KeyExchange::Invalid => {
                return no_store_text(StatusCode::UNAUTHORIZED, "invalid web key");
            }
            KeyExchange::Throttled => {
                return no_store_text(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many attempts; wait a minute and try again",
                );
            }
        }
    }
    match ui.index() {
        Some(bytes) => static_response(bytes, "index.html"),
        None => text(StatusCode::SERVICE_UNAVAILABLE, UI_NOT_BUILT),
    }
}

/// Split the raw query on `&` and take the value of the first segment
/// named exactly `k`; every other segment is kept in order. The page key
/// charset is URL-unreserved, so no percent-decoding is needed.
fn page_key_from_query(query: Option<&str>) -> Option<(String, Vec<&str>)> {
    let mut key: Option<String> = None;
    let mut remaining: Vec<&str> = Vec::new();
    for segment in query?.split('&') {
        let (name, value) = segment.split_once('=').unwrap_or((segment, ""));
        if key.is_none() && name == "k" {
            key = Some(value.to_owned());
        } else {
            remaining.push(segment);
        }
    }
    key.map(|key| (key, remaining))
}

/// 303 back to the root with the `k` segment removed, setting the same
/// session cookie the `/v1/session` exchange builds.
fn redirect_after_key_exchange(
    path: &str,
    remaining: &[&str],
    web_auth: &WebAuth,
    headers: &HeaderMap,
) -> Response {
    let location = if remaining.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", remaining.join("&"))
    };
    let cookie = set_cookie_value(web_auth, host_port(headers));
    let mut response = StatusCode::SEE_OTHER.into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// SPA fallback for non-API GET/HEAD requests: a static file when one
/// exists, the index document for deep links, never HTML for API paths.
pub(crate) async fn handle_fallback(
    Extension(ui): Extension<WebUi>,
    method: Method,
    uri: Uri,
) -> Response {
    let path = uri.path();
    if method != Method::GET && method != Method::HEAD {
        return not_found(path);
    }
    if is_api_path(path) {
        return not_found(path);
    }
    let relative = path.trim_start_matches('/');
    if let Some(bytes) = ui.file(relative) {
        return static_response(bytes, relative);
    }
    if relative.starts_with("assets/") {
        // A stale hashed asset must fail as a missing asset, not as HTML.
        return not_found(path);
    }
    if relative
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'))
    {
        return not_found(path);
    }
    match ui.index() {
        Some(bytes) => static_response(bytes, "index.html"),
        None => text(StatusCode::SERVICE_UNAVAILABLE, UI_NOT_BUILT),
    }
}

/// API paths that must never receive SPA HTML from the fallback.
fn is_api_path(path: &str) -> bool {
    path == "/v1"
        || path == "/health"
        || path == "/models"
        || path.starts_with("/v1/")
        || path.starts_with("/v1beta/")
        || path.starts_with("/models/")
}

/// A plain-text 404. The log carries only the path: a query may hold the
/// page key.
fn not_found(path: &str) -> Response {
    log::warn!("request to unknown path path={path}");
    text(StatusCode::NOT_FOUND, "not found")
}

fn text(status: StatusCode, message: &'static str) -> Response {
    let mut response = Response::new(Body::from(message));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn no_store_text(status: StatusCode, message: &'static str) -> Response {
    let mut response = text(status, message);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// A static UI file with the cache and security headers. `path` is the
/// file's relative path (`index.html` for the document, `assets/...` for
/// hashed assets).
fn static_response(bytes: Cow<'static, [u8]>, path: &str) -> Response {
    let mut response = Response::new(Body::from(bytes.into_owned()));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    let cache = if path == "index.html" {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(INDEX_CSP),
        );
        "no-cache"
    } else if path.starts_with("assets/") {
        ASSETS_CACHE
    } else {
        "no-cache"
    };
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;

    #[test]
    fn content_type_covers_every_extension() {
        for (path, expected) in [
            ("index.html", "text/html; charset=utf-8"),
            ("app.js", "text/javascript; charset=utf-8"),
            ("app.mjs", "text/javascript; charset=utf-8"),
            ("app.css", "text/css; charset=utf-8"),
            ("logo.svg", "image/svg+xml"),
            ("img.png", "image/png"),
            ("img.jpg", "image/jpeg"),
            ("img.jpeg", "image/jpeg"),
            ("img.gif", "image/gif"),
            ("img.webp", "image/webp"),
            ("favicon.ico", "image/x-icon"),
            ("font.woff", "font/woff"),
            ("font.woff2", "font/woff2"),
            ("font.ttf", "font/ttf"),
            ("font.otf", "font/otf"),
            ("data.json", "application/json"),
            ("app.js.map", "application/json"),
            ("notes.txt", "text/plain; charset=utf-8"),
            ("lib.wasm", "application/wasm"),
            ("app.webmanifest", "application/manifest+json"),
            ("file.bin", "application/octet-stream"),
            ("no-extension", "application/octet-stream"),
        ] {
            assert_eq!(content_type(path), expected, "{path}");
        }
    }

    #[test]
    fn from_dir_serves_index_and_assets() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("assets")).unwrap();
        fs::write(dir.path().join("index.html"), b"<html></html>").unwrap();
        fs::write(dir.path().join("assets/app.js"), b"console.log(1)").unwrap();

        let ui = WebUi::from_dir(dir.path());
        assert_eq!(ui.index().unwrap().as_ref(), b"<html></html>");
        assert_eq!(
            ui.file("assets/app.js").unwrap().as_ref(),
            b"console.log(1)"
        );
        // Missing files are missing, not errors.
        assert!(ui.file("assets/nope.js").is_none());
        assert!(ui.file("nope.html").is_none());
    }

    #[test]
    fn dir_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.html"), b"index").unwrap();
        fs::write(outside.path().join("outside.txt"), b"secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join("outside.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();

        let ui = WebUi::from_dir(dir.path());
        assert!(ui.file("../outside.txt").is_none());
        assert!(ui.file("assets/../../outside.txt").is_none());
        assert!(ui.file("/etc/passwd").is_none());
        #[cfg(unix)]
        assert!(ui.file("link.txt").is_none());
    }

    #[test]
    fn disabled_serves_nothing() {
        assert!(WebUi::Disabled.index().is_none());
        assert!(WebUi::Disabled.file("index.html").is_none());
        assert!(WebUi::Disabled.file("assets/app.js").is_none());
    }

    #[test]
    fn from_dir_with_a_nonexistent_root_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let ui = WebUi::from_dir(dir.path().join("nope"));
        assert!(matches!(ui, WebUi::Disabled));
    }
}
