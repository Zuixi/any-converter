//! IPC forwarding for the Accounts/Agents management pages.
//!
//! The webview never speaks HTTP to the embedded server directly: a browser
//! origin crossing into `http://127.0.0.1:<port>` needs a CORS preflight the
//! server does not answer, and `fetch` surfaces that as an opaque
//! `TypeError: Failed to fetch`. Management calls belong to the process that
//! hosts the server, so they travel over Tauri IPC and this module is the
//! single place that knows how to dial the server back.

use serde::Serialize;
use std::sync::OnceLock;
use std::time::Duration;

use crate::db::DesktopDb;
use crate::server_manager::{ServerManager, ServerRunState};

/// How long a management call may take before the IPC layer gives up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared pooled client: pages poll (agents every 10s, sign-ins every 1s),
/// so per-call construction would throw away the connection pool each time.
fn shared_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let mut builder = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
            // Env proxies must never intercept loopback dials.
            if std::env::var("HTTP_PROXY").is_ok() || std::env::var("http_proxy").is_ok() {
                builder = builder.no_proxy();
            }
            builder.build().unwrap_or_default()
        })
        .clone()
}

/// Errors surfaced to the UI, typed so pages can localize them.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyError {
    /// No embedded server task is running and the dial failed. An externally
    /// started server would have answered, so this really means "start it".
    NotRunning,
    /// The manager reports the server as running, but the dial failed.
    Unreachable { message: String },
    /// The method or path is not a valid management request.
    BadRequest,
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

/// Only absolute server paths are forwarded; anything else stays an error
/// instead of becoming an unintended URL.
pub fn validate_path(path: &str) -> Result<&str, ProxyError> {
    if path.starts_with('/') && !path.contains("..") {
        Ok(path)
    } else {
        Err(ProxyError::BadRequest)
    }
}

/// Forward one management call to the server configured in desktop settings.
///
/// Address selection has two sources and one rule: while the manager shows
/// an embedded instance running, dial the address it was spawned with — the
/// settings store may have drifted (a port edited mid-run only applies after
/// a restart). Otherwise fall back to settings, which also reaches a server
/// started outside the app (`any-converter serve`).
///
/// Non-2xx bodies keep the server's own `error.message`/`error.hint` text.
pub async fn forward(
    db: DesktopDb,
    manager: ServerManager,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<serde_json::Value, ProxyError> {
    let method = method.to_ascii_uppercase();
    if !matches!(method.as_str(), "GET" | "POST") {
        return Err(ProxyError::BadRequest);
    }
    let path = validate_path(path)?.to_string();

    let settings = db.settings().map_err(|error| ProxyError::Unreachable {
        message: error.to_string(),
    })?;
    let status = manager.status().await;
    let (host, port) = if status.state == ServerRunState::Running {
        (status.host.clone(), status.port.to_string())
    } else {
        (
            settings
                .get("server.host")
                .cloned()
                .unwrap_or_else(|| "127.0.0.1".to_string()),
            settings
                .get("server.port")
                .cloned()
                .unwrap_or_else(|| "1228".to_string()),
        )
    };
    let api_key = settings.get("server.api_key").cloned();
    let url = format!("http://{}:{}{}", dial_host(&host), port, path);

    let client = shared_client();

    let mut request = if method == "GET" {
        client.get(&url)
    } else {
        client.post(&url)
    };
    if let Some(key) = api_key.as_deref() {
        request = request.bearer_auth(key);
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
            return Err(if status.state == ServerRunState::Running {
                ProxyError::Unreachable {
                    message: error.to_string(),
                }
            } else {
                ProxyError::NotRunning
            });
        }
    };

    let status = response.status();
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
    serde_json::from_str(&text).map_err(|_| ProxyError::Http {
        message: format!("HTTP {status}"),
    })
}

/// Extract `error.message` / `error.hint` from a server error body, matching
/// what the pages used to pull out of `fetch` responses.
fn error_message(body: &str, status: u16) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
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
    fn validate_path_rejects_relative_and_traversal() {
        assert!(validate_path("/v1/accounts").is_ok());
        assert!(validate_path("v1/accounts").is_err());
        assert!(validate_path("/v1/../secrets").is_err());
        assert!(validate_path("").is_err());
    }

    #[test]
    fn error_message_joins_message_and_hint() {
        let body = r#"{"error":{"message":"nope","hint":"retry later"}}"#;
        assert_eq!(error_message(body, 400), "nope — retry later");
        assert_eq!(error_message("not json", 500), "HTTP 500");
        assert_eq!(error_message("{}", 404), "HTTP 404");
    }
}
