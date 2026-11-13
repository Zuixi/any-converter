//! IPC forwarding for the Accounts/Agents management pages.
//!
//! The webview never speaks HTTP to the embedded server directly: a browser
//! origin crossing into `http://127.0.0.1:<port>` needs a CORS preflight the
//! server does not answer, and `fetch` surfaces that as an opaque
//! `TypeError: Failed to fetch`. Management calls belong to the process that
//! hosts the server, so they travel over Tauri IPC and this module is the
//! single place that knows how to dial the server back.

use any_converter_server::home::Home;
use any_converter_server::secrets::ApiKeyStore;
use serde::Serialize;
use std::sync::OnceLock;
use std::time::Duration;

use crate::server_manager::{ServerManager, ServerRunState};

/// How long a management call may take before the IPC layer gives up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared pooled client: pages poll (agents every 10s, sign-ins every 1s),
/// so per-call construction would throw away the connection pool each time.
fn shared_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                // Loopback management traffic must never be redirected by
                // HTTP_PROXY, HTTPS_PROXY, or ALL_PROXY.
                .no_proxy()
                .build()
                .unwrap_or_default()
        })
        .clone()
}

/// Errors surfaced to the UI, typed so pages can localize them.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyError {
    /// No embedded server task is running. The proxy fails closed instead of
    /// dialing an unverified external server.
    NotRunning,
    /// The manager reports the server as running, but the dial failed.
    Unreachable { message: String },
    /// The method or path is not a valid management request.
    BadRequest,
    /// The management key is unavailable from the system keychain.
    KeyUnavailable { message: String },
    /// The server answered non-2xx; `message` carries its error text.
    Http { message: String },
}

/// A bind host rewritten to something dialable from this process. A wildcard
/// bind cannot be dialed back; loopback always can.
pub fn dial_host(host: &str) -> String {
    match host {
        "0.0.0.0" | "::" | "*" => "127.0.0.1".to_string(),
        other => {
            // Bracket bare IPv6 literals so they survive URL parsing.
            if other.contains(':') && !other.starts_with('[') {
                format!("[{other}]")
            } else {
                other.to_string()
            }
        }
    }
}

/// Permit exactly the management routes the Desktop UI uses.
///
/// This is deliberately not a generic local HTTP proxy. Dynamic identifiers
/// are constrained to characters that are safe in one URL segment; the server
/// remains responsible for semantic validation.
pub fn validate_request(method: &str, path: &str) -> Result<(), ProxyError> {
    let method = method.to_ascii_uppercase();
    // The query string is opaque and forwarded untouched; only the path
    // segments participate in the allowlist.
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let segments = path
        .strip_prefix('/')
        .unwrap_or_default()
        .split('/')
        .collect::<Vec<_>>();

    let valid = match (method.as_str(), segments.as_slice()) {
        ("GET", ["v1", "accounts"]) => true,
        ("GET", ["v1", "accounts", "quotas"]) => true,
        ("GET", ["v1", "accounts", "plugins"]) => true,
        ("GET", ["v1", "accounts", id, "models"]) => safe_segment(id),
        ("POST", ["v1", "accounts", "order"]) => true,
        (
            "POST",
            [
                "v1",
                "accounts",
                id,
                action @ ("adopt" | "release" | "remove"),
            ],
        ) => safe_segment(id) && !action.is_empty(),
        ("POST", ["v1", "accounts", "signin"]) => true,
        ("GET", ["v1", "accounts", "signin", id]) => safe_segment(id),
        (
            "POST",
            [
                "v1",
                "accounts",
                "signin",
                id,
                action @ ("submit" | "cancel"),
            ],
        ) => safe_segment(id) && !action.is_empty(),
        ("GET", ["v1", "agents"]) => true,
        ("GET", ["v1", "agents", id, "keys"]) => safe_segment(id),
        (
            "POST",
            [
                "v1",
                "agents",
                id,
                action @ ("connect" | "disconnect" | "sync"),
            ],
        ) => safe_segment(id) && !action.is_empty(),
        ("POST", ["v1", "agents", id, "key", "rotate"]) => safe_segment(id),
        ("GET", ["v1", "admin", "config"]) => true,
        ("PATCH", ["v1", "admin", "config"]) => true,
        ("GET", ["v1", "admin", "server", "status"]) => true,
        ("GET", ["v1", "admin", "logs"]) => true,
        ("GET", ["v1", "admin", "usage"]) => true,
        ("GET", ["v1", "admin", "status"]) => true,
        ("POST", ["v1", "admin", "convert"]) => true,
        // Provider model discovery and the exposure catalog (the providers
        // page's "fetch models" flows). Without these the page's calls die
        // at this allowlist and surface as a bare "Invalid request".
        ("POST", ["v1", "admin", "providers", "discover"]) => true,
        ("GET", ["v1", "admin", "providers", id, "models"]) => safe_segment(id),
        ("GET", ["v1", "admin", "catalog"]) => true,
        ("PUT", ["v1", "admin", "secrets", "provider", provider]) => safe_segment(provider),
        ("DELETE", ["v1", "admin", "secrets", "provider", provider]) => safe_segment(provider),
        ("PUT", ["v1", "admin", "secrets", "inference"]) => true,
        ("DELETE", ["v1", "admin", "secrets", "inference"]) => true,
        ("POST", ["v1", "admin", "secrets", "management", "rotate"]) => true,
        _ => false,
    };

    valid.then_some(()).ok_or(ProxyError::BadRequest)
}

fn safe_segment(segment: &str) -> bool {
    let decoded = match percent_decode(segment) {
        Some(decoded) => decoded,
        None => return false,
    };
    !decoded.is_empty()
        && decoded != "."
        && decoded != ".."
        && decoded.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | '~' | ':' | '@')
        })
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }

        let high = bytes.get(index + 1)?;
        let low = bytes.get(index + 2)?;
        decoded.push((hex_value(*high)? << 4) | hex_value(*low)?);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Forward one management call to the embedded server.
///
/// Requests are accepted only while the manager observes the embedded instance
/// as running. A stopped instance fails closed instead of dialing a stale
/// settings address or an unrelated external server.
///
/// Non-2xx bodies keep the server's own `error.message`/`error.hint` text.
pub async fn forward(
    manager: ServerManager,
    home: &Home,
    method: &str,
    path: &str,
    body: Option<&str>,
    if_match: Option<&str>,
) -> Result<serde_json::Value, ProxyError> {
    let method = method.to_ascii_uppercase();
    if !matches!(method.as_str(), "GET" | "POST" | "PATCH" | "PUT" | "DELETE") {
        return Err(ProxyError::BadRequest);
    }
    validate_request(&method, path)?;

    let status = manager.status().await;
    // `Attached` is a gateway this process did not start but just proved
    // alive at the metadata address — management traffic rides the same
    // loopback dial either way.
    if !matches!(
        status.state,
        ServerRunState::Running | ServerRunState::Attached
    ) {
        return Err(ProxyError::NotRunning);
    }

    let api_key = ApiKeyStore::for_home(home)
        .get_management()
        .map_err(|error| ProxyError::KeyUnavailable {
            message: error.to_string(),
        })?;
    let host = status.host.clone();
    let port = status.port.to_string();
    let url = format!("http://{}:{}{}", dial_host(&host), port, path);

    let client = shared_client();

    let mut request = client.request(method.parse().map_err(|_| ProxyError::BadRequest)?, &url);
    request = request.bearer_auth(api_key);
    if let Some(revision) = if_match.filter(|value| !value.is_empty()) {
        request = request.header(reqwest::header::IF_MATCH, revision);
    }
    if let Some(payload) = body.filter(|text| !text.is_empty()) {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload.to_string());
    }

    let response = match request.send().await {
        Ok(response) => response,
        // Connection-level failure: name the likely cause instead of letting
        // it surface as an opaque transport error.
        Err(error) => {
            return Err(ProxyError::Unreachable {
                message: error.to_string(),
            });
        }
    };

    let status = response.status();
    // Capture the content type before the body is consumed: a 2xx non-JSON
    // body (the ledger CSV) must resolve to its raw text, not parse as JSON.
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let text = response
        .text()
        .await
        .map_err(|error| ProxyError::Unreachable {
            message: error.to_string(),
        })?;
    if !status.is_success() {
        return Err(ProxyError::Http {
            message: error_message(&text, status.as_u16()),
        });
    }
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let is_json = content_type
        .as_deref()
        .is_some_and(|content_type| content_type.starts_with("application/json"));
    if is_json {
        serde_json::from_str(&text).map_err(|_| ProxyError::Http {
            message: format!("HTTP {status}"),
        })
    } else {
        Ok(serde_json::Value::String(text))
    }
}

/// Extract the server's error text from an error body, matching what the
/// pages used to pull out of `fetch` responses. The data API's string shape
/// (`{"error": "..."}`) passes through verbatim; the object shape joins
/// `message — hint`.
fn error_message(body: &str, status: u16) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(message) = value["error"].as_str() {
            return message.to_string();
        }
        let error = &value["error"];
        let parts: Vec<&str> = [error["message"].as_str(), error["hint"].as_str()]
            .into_iter()
            .flatten()
            .collect();
        if !parts.is_empty() {
            return parts.join(" — ");
        }
    }
    format!("HTTP {status}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn dial_host_rewrites_wildcards_and_brackets_ipv6() {
        assert_eq!(dial_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(dial_host("::"), "127.0.0.1");
        assert_eq!(dial_host("::1"), "[::1]");
        assert_eq!(dial_host("[::1]"), "[::1]");
        assert_eq!(dial_host("localhost"), "localhost");
        assert_eq!(dial_host("192.168.1.4"), "192.168.1.4");
    }

    #[test]
    fn validate_request_accepts_only_management_routes() {
        assert!(validate_request("GET", "/v1/accounts").is_ok());
        assert!(validate_request("POST", "/v1/accounts/order").is_ok());
        assert!(validate_request("GET", "/v1/accounts/id/models").is_ok());
        assert!(validate_request("PATCH", "/v1/admin/config").is_ok());
        assert!(validate_request("PUT", "/v1/admin/secrets/provider/openai").is_ok());

        assert!(validate_request("GET", "v1/accounts").is_err());
        assert!(validate_request("GET", "/v1/../secrets").is_err());
        assert!(validate_request("GET", "/health").is_err());
        assert!(validate_request("DELETE", "/v1/accounts").is_err());
        assert!(validate_request("GET", "/v1/accounts/../models").is_err());
        assert!(validate_request("GET", "/v1/accounts/a%2Fb/models").is_err());
        assert!(validate_request("GET", "/v1/accounts/a%252Fb/models").is_err());
        assert!(validate_request("GET", "/v1/accounts/a%2/models").is_err());
        assert!(validate_request("GET", "/v1/accounts/a%00b/models").is_err());
        assert!(validate_request("GET", "/v1/accounts/user%40example.test/models").is_ok());
    }

    #[test]
    fn validate_request_accepts_the_ui_data_routes() {
        assert!(validate_request("GET", "/v1/admin/logs").is_ok());
        assert!(validate_request("GET", "/v1/admin/usage?ledger=1&format=csv").is_ok());
        assert!(validate_request("GET", "/v1/admin/status").is_ok());
        assert!(validate_request("POST", "/v1/admin/convert").is_ok());

        assert!(validate_request("GET", "/v1/admin/usage/../config").is_err());
        assert!(validate_request("GET", "/health").is_err());
        assert!(validate_request("POST", "/v1/admin/status").is_err());
    }

    /// The providers page's model flows: unsaved probe, saved refresh, and
    /// the catalog. Each of these was once missing from the allowlist and
    /// surfaced to users as a bare "Invalid request".
    #[test]
    fn validate_request_accepts_the_provider_model_routes() {
        assert!(validate_request("POST", "/v1/admin/providers/discover").is_ok());
        assert!(validate_request("GET", "/v1/admin/providers/mm/models").is_ok());
        assert!(validate_request("GET", "/v1/admin/providers/mm/models?refresh=1").is_ok());
        assert!(validate_request("GET", "/v1/admin/catalog").is_ok());

        // Mutations and path tricks stay out.
        assert!(validate_request("DELETE", "/v1/admin/providers/mm/models").is_err());
        assert!(validate_request("GET", "/v1/admin/providers/../secrets").is_err());
        assert!(validate_request("GET", "/v1/admin/providers/a%2Fb/models").is_err());
        assert!(validate_request("POST", "/v1/admin/catalog").is_err());
    }

    #[test]
    fn error_message_joins_message_and_hint() {
        let body = r#"{"error":{"message":"nope","hint":"retry later"}}"#;
        assert_eq!(error_message(body, 400), "nope — retry later");
        assert_eq!(error_message("not json", 500), "HTTP 500");
        assert_eq!(error_message("{}", 404), "HTTP 404");
    }

    #[test]
    fn error_message_passes_the_data_api_string_through_verbatim() {
        let body = r#"{"error":"limit must be a non-negative integer","code":"invalid_limit"}"#;
        assert_eq!(
            error_message(body, 400),
            "limit must be a non-negative integer"
        );
    }
}
