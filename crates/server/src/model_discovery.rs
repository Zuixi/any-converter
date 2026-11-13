//! Upstream model discovery for API-key providers.
//!
//! The gateway can only promise that a model is usable when it knows both
//! what the provider offers and what the gateway exposes. This module owns
//! the first half: ask the provider's model-list endpoint with the same
//! credential and wire format that generation requests use.
//!
//! Discovery never persists anything and never logs a credential. Failures
//! are reports, not errors, so a provider can still be saved when its
//! vendor has no list endpoint.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use any_converter_core::convert::Format;
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde_json::Value;

use crate::config::AuthScheme;

/// How long a cached discovery stays authoritative.
pub const CACHE_TTL: Duration = Duration::from_secs(600);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_MODELS: usize = 2000;
const ERROR_BODY_PREVIEW: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryStatus {
    Ok,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryErrorKind {
    MissingApiKey,
    BlockedAddress,
    InvalidBaseUrl,
    InvalidCredentials,
    EndpointNotFound,
    UnsupportedFormat,
    Timeout,
    Network,
    RateLimited,
    MalformedResponse,
    ResponseTooLarge,
    HttpError,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryError {
    pub kind: DiscoveryErrorKind,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryReport {
    pub status: DiscoveryStatus,
    pub source: String,
    pub checked_at_unix: u64,
    pub latency_ms: u64,
    pub models: Vec<DiscoveredModel>,
    pub count: usize,
    pub error: Option<DiscoveryError>,
}

impl DiscoveryReport {
    fn ok(models: Vec<DiscoveredModel>, latency: Duration) -> Self {
        Self {
            status: DiscoveryStatus::Ok,
            source: "upstream".to_owned(),
            checked_at_unix: now_unix(),
            latency_ms: latency_millis(latency),
            count: models.len(),
            models,
            error: None,
        }
    }

    fn unsupported(
        kind: DiscoveryErrorKind,
        message: impl Into<String>,
        hint: Option<&str>,
        latency: Duration,
    ) -> Self {
        Self {
            status: DiscoveryStatus::Unsupported,
            source: "upstream".to_owned(),
            checked_at_unix: now_unix(),
            latency_ms: latency_millis(latency),
            models: Vec::new(),
            count: 0,
            error: Some(DiscoveryError {
                kind,
                message: message.into(),
                hint: hint.map(str::to_owned),
            }),
        }
    }

    fn failure(
        kind: DiscoveryErrorKind,
        message: impl Into<String>,
        hint: Option<&str>,
        latency: Duration,
    ) -> Self {
        Self {
            status: DiscoveryStatus::Error,
            source: "upstream".to_owned(),
            checked_at_unix: now_unix(),
            latency_ms: latency_millis(latency),
            models: Vec::new(),
            count: 0,
            error: Some(DiscoveryError {
                kind,
                message: message.into(),
                hint: hint.map(str::to_owned),
            }),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.status == DiscoveryStatus::Ok
    }

    /// A report for providers whose models cannot come from a key-based
    /// list endpoint (plugin and account-signed providers).
    pub fn unsupported_now(
        kind: DiscoveryErrorKind,
        message: impl Into<String>,
        hint: Option<&str>,
    ) -> Self {
        Self::unsupported(kind, message, hint, Duration::ZERO)
    }
}

/// Ask one provider which models it offers.
///
/// `api_key` is only used for the outbound request. It is redacted from
/// error messages and never included in the returned report.
pub async fn discover(
    format: Format,
    base_url: &str,
    api_key: &str,
    auth_headers: Option<&[(String, String)]>,
    proxy: Option<&str>,
) -> DiscoveryReport {
    let started = Instant::now();
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return DiscoveryReport::failure(
            DiscoveryErrorKind::MissingApiKey,
            "no API key is configured for this provider",
            Some("add the provider's API key, then test again"),
            started.elapsed(),
        );
    }
    if let Err(error) = validate_base_url(base_url) {
        return DiscoveryReport::failure(
            error.kind,
            error.message,
            error.hint.as_deref(),
            started.elapsed(),
        );
    }
    let url = match models_url(format, base_url) {
        Ok(url) => url,
        Err(error) => {
            return DiscoveryReport::failure(
                error.kind,
                error.message,
                error.hint.as_deref(),
                started.elapsed(),
            );
        }
    };
    let client = match build_client(proxy) {
        Ok(client) => client,
        Err(error) => {
            return DiscoveryReport::failure(
                DiscoveryErrorKind::Network,
                format!("could not build the discovery HTTP client: {error}"),
                Some("check the provider or server proxy setting"),
                started.elapsed(),
            );
        }
    };

    // A saved provider passes the exact headers generation uses, so a vendor
    // that wants `Authorization: Bearer` instead of `x-api-key` discovers
    // models with the same credential style.
    let configured = auth_headers.filter(|headers| !headers.is_empty());
    let primary = configured
        .map(<[(String, String)]>::to_vec)
        .unwrap_or_else(|| default_auth_headers(format, base_url, api_key));
    let send = |headers: &[(String, String)]| {
        let mut request = client.get(url.clone());
        request = request.header("accept", "application/json");
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        // No `limit`: some Anthropic-compatible gateways reject an
        // out-of-range page size with a 400 instead of clamping. Gemini's
        // documented maximum is safe to request.
        if format == Format::Gemini {
            request = request.query(&[("pageSize", "1000")]);
        }
        request.send()
    };

    let mut response = match send(&primary).await {
        Ok(response) => response,
        Err(error) => {
            let (kind, hint) = classify_transport_error(&error);
            return DiscoveryReport::failure(
                kind,
                redact(&error.to_string(), api_key),
                hint,
                started.elapsed(),
            );
        }
    };
    let mut status = response.status();
    // Some Anthropic-compatible gateways sign their model-list endpoint
    // differently from generation (MiniMax documents `X-Api-Key` for the
    // list and requires `Authorization: Bearer` for messages). A read-only
    // probe can afford one retry with the other common scheme before we
    // tell the user their key is wrong.
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        && let Some(alternate) = alternate_auth_headers(format, api_key, &primary)
        && let Ok(retry) = send(&alternate).await
        && retry.status().is_success()
    {
        response = retry;
        status = response.status();
    }
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return DiscoveryReport::failure(
                DiscoveryErrorKind::Network,
                redact(&error.to_string(), api_key),
                Some("check the Base URL and network access"),
                started.elapsed(),
            );
        }
    };
    let latency = started.elapsed();

    if bytes.len() > MAX_BODY_BYTES {
        return DiscoveryReport::failure(
            DiscoveryErrorKind::ResponseTooLarge,
            "the provider's model list is too large to read safely",
            None,
            latency,
        );
    }
    let body = String::from_utf8_lossy(&bytes);
    if matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::GONE
    ) {
        return DiscoveryReport::unsupported(
            DiscoveryErrorKind::EndpointNotFound,
            format!("the provider returned {status} for its model list"),
            Some("this vendor has no model list endpoint; add model IDs manually"),
            latency,
        );
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return DiscoveryReport::failure(
            DiscoveryErrorKind::InvalidCredentials,
            format!("the provider rejected the API key ({status})"),
            Some("check that the key is valid and allowed to list models"),
            latency,
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return DiscoveryReport::failure(
            DiscoveryErrorKind::RateLimited,
            "the provider rate-limited the model list request",
            Some("wait a moment and test again"),
            latency,
        );
    }
    if !status.is_success() {
        return DiscoveryReport::failure(
            DiscoveryErrorKind::HttpError,
            redact(
                &format!("the provider returned {status}: {}", preview(&body)),
                api_key,
            ),
            Some("check the Base URL and provider status page"),
            latency,
        );
    }

    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return DiscoveryReport::failure(
                DiscoveryErrorKind::MalformedResponse,
                format!("the provider returned malformed JSON: {error}"),
                Some("check that the Base URL points at the provider's API root"),
                latency,
            );
        }
    };
    match parse_models(format, &value) {
        Ok(models) => {
            if models.is_empty() {
                DiscoveryReport::unsupported(
                    DiscoveryErrorKind::EndpointNotFound,
                    "the provider's model list is empty",
                    Some("add model IDs manually if this provider does not enumerate them"),
                    latency,
                )
            } else {
                DiscoveryReport::ok(models, latency)
            }
        }
        Err(message) => DiscoveryReport::failure(
            DiscoveryErrorKind::MalformedResponse,
            message,
            Some("the response did not contain a model list this format understands"),
            latency,
        ),
    }
}

/// Normalize a provider base URL into its model-list endpoint.
///
/// Custom base URLs may already carry a path prefix (`/anthropic`,
/// `/openai/v1`); only the format's version segment is de-duplicated.
pub fn models_url(format: Format, base_url: &str) -> Result<String, DiscoveryError> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(invalid_base_url("the Base URL is empty"));
    }
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(invalid_base_url(
            "the Base URL must start with http:// or https://",
        ));
    }
    if base.ends_with("/models") {
        return Ok(base.to_owned());
    }
    let (version, path) = match format {
        Format::OpenAIChat | Format::OpenAIResponses | Format::Claude => ("/v1", "/v1/models"),
        Format::Gemini => ("/v1beta", "/v1beta/models"),
    };
    if base.ends_with(version) {
        Ok(format!("{base}/models"))
    } else {
        Ok(format!("{base}{path}"))
    }
}

/// The headers a format signs with when the provider has no override.
///
/// Mirrors the generation path exactly: an Anthropic-compatible gateway
/// (anything but `api.anthropic.com`) signs with Bearer, OAuth tokens sign
/// with Bearer on any host, and the Claude wire always carries
/// `anthropic-version`.
fn default_auth_headers(format: Format, base_url: &str, api_key: &str) -> Vec<(String, String)> {
    let scheme =
        if format == Format::Claude && crate::credentials::is_anthropic_oauth_token(api_key) {
            AuthScheme::Bearer
        } else {
            AuthScheme::for_endpoint(format, base_url)
        };
    crate::auth::build_provider_upstream_headers(format, scheme, api_key, &[])
}

/// The other common signing style for a format, used once when a read-only
/// probe is rejected. Only Claude has two widely used styles: Anthropic's
/// `x-api-key` and the `Authorization: Bearer` that compatible gateways
/// such as MiniMax require for generation. Non-credential headers from
/// `tried` (tenant ids, `anthropic-version`) are preserved.
fn alternate_auth_headers(
    format: Format,
    api_key: &str,
    tried: &[(String, String)],
) -> Option<Vec<(String, String)>> {
    if format != Format::Claude {
        return None;
    }
    let is_credential_header = |name: &str| {
        name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("x-api-key")
            || name.eq_ignore_ascii_case("api-key")
            || name.eq_ignore_ascii_case("x-goog-api-key")
    };
    let used_bearer = tried
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
    let mut headers: Vec<(String, String)> = tried
        .iter()
        .filter(|(name, _)| !is_credential_header(name))
        .cloned()
        .collect();
    if used_bearer {
        headers.push(("x-api-key".to_owned(), api_key.to_owned()));
    } else {
        headers.push(("Authorization".to_owned(), format!("Bearer {api_key}")));
    }
    Some(headers)
}

/// Pull the model rows out of a provider's list payload.
///
/// OpenAI-style envelopes say `data`, Gemini says `models`, and relays
/// improvise (`result`, `items`, or a bare array). The canonical keys are
/// tried in order, descending recursively, so a nested envelope still yields
/// its rows.
fn extract_model_rows(node: &Value) -> Option<&Vec<Value>> {
    if let Some(array) = node.as_array() {
        return Some(array);
    }
    for key in ["data", "models", "result", "items"] {
        if let Some(rows) = node.get(key).and_then(extract_model_rows) {
            return Some(rows);
        }
    }
    None
}

fn parse_models(format: Format, value: &Value) -> Result<Vec<DiscoveredModel>, String> {
    let rows = extract_model_rows(value).ok_or_else(|| {
        "response has no model list (expected a `data`/`models` array)".to_owned()
    })?;
    let mut by_id: HashMap<String, DiscoveredModel> = HashMap::new();
    match format {
        Format::OpenAIChat | Format::OpenAIResponses | Format::Claude => {
            for row in rows {
                let Some(id) = row.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let id = id.trim();
                if id.is_empty() {
                    continue;
                }
                // Anthropic says `display_name`; relays often say `name`.
                let display_name = row
                    .get("display_name")
                    .or_else(|| row.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                by_id
                    .entry(id.to_owned())
                    .or_insert_with(|| DiscoveredModel {
                        id: id.to_owned(),
                        display_name,
                        capabilities: vec!["chat".to_owned()],
                    });
            }
        }
        Format::Gemini => {
            for row in rows {
                // Native Gemini rows say `name: "models/<id>"`; OpenAI-ish
                // relays put a bare id in `id`.
                let Some(raw) = row
                    .get("name")
                    .or_else(|| row.get("id"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let id = raw.strip_prefix("models/").unwrap_or(raw).trim();
                if id.is_empty() {
                    continue;
                }
                let supports_chat = row
                    .get("supportedGenerationMethods")
                    .and_then(Value::as_array)
                    .is_none_or(|methods| {
                        methods
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|method| method == "generateContent")
                    });
                if !supports_chat {
                    continue;
                }
                let display_name = row
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                by_id
                    .entry(id.to_owned())
                    .or_insert_with(|| DiscoveredModel {
                        id: id.to_owned(),
                        display_name,
                        capabilities: vec!["chat".to_owned()],
                    });
            }
        }
    }

    let mut models: Vec<_> = by_id.into_values().collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.truncate(MAX_MODELS);
    Ok(models)
}

fn build_client(proxy: Option<&str>) -> Result<Client, reqwest::Error> {
    let builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    match proxy.filter(|value| !value.trim().is_empty()) {
        Some(url) => builder.no_proxy().proxy(reqwest::Proxy::all(url)?).build(),
        None => builder.build(),
    }
}

/// Reject URLs the management API must never make the gateway fetch.
///
/// Loopback and private addresses stay allowed: local inference servers
/// (Ollama, LM Studio) are a first-class provider kind. Link-local ranges
/// are blocked because they host cloud metadata endpoints.
fn validate_base_url(raw: &str) -> Result<(), DiscoveryError> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|error| invalid_base_url(&format!("invalid Base URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid_base_url(
            "the Base URL must use http:// or https://",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid_base_url("the Base URL has no host"))?;
    if host.eq_ignore_ascii_case("metadata.google.internal")
        || host.eq_ignore_ascii_case("metadata")
    {
        return Err(blocked_address());
    }
    let blocked = match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            ip.is_unspecified() || ip.is_multicast() || ip.is_link_local() || ip.is_broadcast()
        }
        Ok(IpAddr::V6(ip)) => {
            ip.is_unspecified() || ip.is_multicast() || ip.is_unicast_link_local()
        }
        Err(_) => false,
    };
    if blocked {
        return Err(blocked_address());
    }
    Ok(())
}

fn invalid_base_url(message: &str) -> DiscoveryError {
    DiscoveryError {
        kind: DiscoveryErrorKind::InvalidBaseUrl,
        message: message.to_owned(),
        hint: Some("use the provider's API root, for example https://api.openai.com".to_owned()),
    }
}

fn blocked_address() -> DiscoveryError {
    DiscoveryError {
        kind: DiscoveryErrorKind::BlockedAddress,
        message: "the Base URL points at a blocked link-local address".to_owned(),
        hint: Some("cloud metadata addresses are not allowed".to_owned()),
    }
}

fn classify_transport_error(error: &reqwest::Error) -> (DiscoveryErrorKind, Option<&'static str>) {
    if error.is_timeout() {
        (
            DiscoveryErrorKind::Timeout,
            Some("check the Base URL, proxy, and network access"),
        )
    } else if error.is_connect() {
        (
            DiscoveryErrorKind::Network,
            Some("the provider did not accept the connection"),
        )
    } else {
        (
            DiscoveryErrorKind::Network,
            Some("check the Base URL, proxy, and network access"),
        )
    }
}

fn preview(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= ERROR_BODY_PREVIEW {
        trimmed.to_owned()
    } else {
        let preview: String = trimmed.chars().take(ERROR_BODY_PREVIEW).collect();
        format!("{preview}…")
    }
}

/// Remove a credential from any provider-controlled error text.
fn redact(message: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        message.to_owned()
    } else {
        message.replace(api_key, "<redacted>")
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn latency_millis(latency: Duration) -> u64 {
    u64::try_from(latency.as_millis()).unwrap_or(u64::MAX)
}

/// Build the cache key for a saved provider's discovery result.
///
/// The signing scheme is part of the key: switching a provider from
/// `x-api-key` to `bearer` must not keep serving the old probe's result.
pub fn cache_key(name: &str, format: Format, base_url: &str, scheme: AuthScheme) -> String {
    format!(
        "{name}|{}|{}|{}",
        format.as_str(),
        base_url.trim_end_matches('/'),
        scheme.as_str()
    )
}

/// In-memory, TTL-bounded discovery results.
///
/// This is deliberately not persisted: upstream catalogs are derived data
/// and must not bloat or stale the canonical config file.
#[derive(Default)]
pub struct DiscoveryCache {
    entries: Mutex<HashMap<String, CachedReport>>,
}

struct CachedReport {
    report: DiscoveryReport,
    stored: Instant,
}

impl DiscoveryCache {
    pub fn get(&self, key: &str) -> Option<DiscoveryReport> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(key)?;
        (entry.stored.elapsed() <= CACHE_TTL).then(|| entry.report.clone())
    }

    pub fn put(&self, key: String, report: DiscoveryReport) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                key,
                CachedReport {
                    report,
                    stored: Instant::now(),
                },
            );
        }
    }

    pub fn invalidate(&self, key: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode, header},
        response::{IntoResponse, Response},
        routing::get,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn model_list_urls_normalize_version_segments() {
        assert_eq!(
            models_url(Format::OpenAIChat, "https://api.deepseek.com").unwrap(),
            "https://api.deepseek.com/v1/models"
        );
        assert_eq!(
            models_url(Format::OpenAIChat, "https://api.deepseek.com/v1/").unwrap(),
            "https://api.deepseek.com/v1/models"
        );
        assert_eq!(
            models_url(Format::Claude, "https://api.minimax.cn/anthropic").unwrap(),
            "https://api.minimax.cn/anthropic/v1/models"
        );
        assert_eq!(
            models_url(Format::Gemini, "https://generativelanguage.googleapis.com").unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
        assert_eq!(
            models_url(Format::Gemini, "https://example.test/v1beta").unwrap(),
            "https://example.test/v1beta/models"
        );
    }

    #[test]
    fn parses_openai_and_claude_data_lists() {
        let openai = json!({"data": [{"id": "gpt-4.1"}, {"id": "gpt-4.1"}]});
        let models = parse_models(Format::OpenAIChat, &openai).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-4.1");

        let claude =
            json!({"data": [{"id": "claude-sonnet-4", "display_name": "Claude Sonnet 4"}]});
        let models = parse_models(Format::Claude, &claude).unwrap();
        assert_eq!(models[0].display_name.as_deref(), Some("Claude Sonnet 4"));
    }

    /// Relays improvise envelopes: `models`, `result`, `items`, or a bare
    /// array must all yield their rows (oh-my-pi's tolerant extraction).
    #[test]
    fn parses_improvised_model_list_envelopes() {
        for payload in [
            json!({"models": [{"id": "m-1"}]}),
            json!({"result": [{"id": "m-1"}]}),
            json!({"items": [{"id": "m-1"}]}),
            json!([{"id": "m-1"}]),
            json!({"data": {"models": [{"id": "m-1"}]}}),
        ] {
            let models = parse_models(Format::OpenAIChat, &payload).unwrap();
            assert_eq!(models.len(), 1, "payload {payload} yielded {models:?}");
            assert_eq!(models[0].id, "m-1");
        }

        // Relays that only carry a `name` per row still produce usable ids.
        let named = json!({"models": [{"id": "m-1", "name": "Model One"}]});
        let models = parse_models(Format::OpenAIChat, &named).unwrap();
        assert_eq!(models[0].display_name.as_deref(), Some("Model One"));
    }

    #[test]
    fn parses_gemini_models_and_filters_non_chat_methods() {
        let gemini = json!({"models": [
            {"name": "models/gemini-2.0-flash", "displayName": "Gemini 2.0 Flash", "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/text-embedding-004", "supportedGenerationMethods": ["embedContent"]}
        ]});
        let models = parse_models(Format::Gemini, &gemini).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-2.0-flash");
    }

    #[test]
    fn blocks_metadata_and_link_local_addresses_but_allows_local_inference() {
        assert!(validate_base_url("http://169.254.169.254/latest").is_err());
        assert!(validate_base_url("http://metadata.google.internal").is_err());
        assert!(validate_base_url("ftp://example.test").is_err());
        assert!(validate_base_url("http://127.0.0.1:11434").is_ok());
        assert!(validate_base_url("http://192.168.1.10:8080").is_ok());
        assert!(validate_base_url("https://api.openai.com").is_ok());
    }

    #[test]
    fn error_text_never_echoes_the_api_key() {
        assert_eq!(
            redact("upstream rejected sk-secret", "sk-secret"),
            "upstream rejected <redacted>"
        );
    }

    #[test]
    fn alternate_claude_headers_swap_scheme_and_keep_extras() {
        let tried = vec![
            ("X-Tenant".to_owned(), "acme".to_owned()),
            ("x-api-key".to_owned(), "sk-test".to_owned()),
            ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
        ];
        let alternate = alternate_auth_headers(Format::Claude, "sk-test", &tried).unwrap();
        assert!(alternate.contains(&("X-Tenant".to_owned(), "acme".to_owned())));
        // The version header is a wire concern, not a credential: it rides
        // along on the retry instead of being stripped.
        assert_eq!(
            alternate
                .iter()
                .filter(|(name, _)| name == "anthropic-version")
                .count(),
            1
        );
        assert!(
            alternate
                .iter()
                .any(|(name, value)| name == "Authorization" && value == "Bearer sk-test")
        );
        assert!(!alternate.iter().any(|(name, _)| name == "x-api-key"));

        assert!(alternate_auth_headers(Format::OpenAIChat, "sk-test", &tried).is_none());
    }

    /// The format default for an Anthropic-compatible gateway (anything but
    /// `api.anthropic.com`) is Bearer, and the Claude wire keeps its version
    /// header; OAuth tokens sign Bearer even on the official host.
    #[test]
    fn default_headers_follow_the_endpoint_and_credential_kind() {
        let gateway = default_auth_headers(
            Format::Claude,
            "https://api.minimax.cn/anthropic",
            "sk-test",
        );
        assert_eq!(
            gateway,
            vec![
                ("Authorization".to_owned(), "Bearer sk-test".to_owned()),
                ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
            ]
        );

        let official = default_auth_headers(Format::Claude, "https://api.anthropic.com", "sk-test");
        assert_eq!(
            official,
            vec![
                ("x-api-key".to_owned(), "sk-test".to_owned()),
                ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
            ]
        );

        let oauth = default_auth_headers(
            Format::Claude,
            "https://api.anthropic.com",
            "sk-ant-oat12-abc",
        );
        assert_eq!(
            oauth,
            vec![
                (
                    "Authorization".to_owned(),
                    "Bearer sk-ant-oat12-abc".to_owned()
                ),
                ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
            ]
        );
    }

    async fn openai_models(headers: HeaderMap) -> Response {
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer sk-test")
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Json(json!({"data": [{"id": "model-a"}, {"id": "model-b"}]})).into_response()
    }

    #[tokio::test]
    async fn discovers_models_from_a_live_upstream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/v1/models", get(openai_models));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let report = discover(
            Format::OpenAIChat,
            &format!("http://{address}"),
            "sk-test",
            None,
            None,
        )
        .await;
        assert_eq!(report.status, DiscoveryStatus::Ok);
        assert_eq!(report.count, 2);
        assert_eq!(report.models[1].id, "model-b");
    }

    /// Anthropic-compatible gateways may want `Authorization: Bearer` on a
    /// `claude`-format provider. Discovery must sign with the provider's
    /// resolved headers, not the format default, or the probe 401s exactly
    /// like generation used to.
    #[tokio::test]
    async fn provider_auth_headers_override_the_format_default() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/v1/models", get(openai_models));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let bearer = vec![("Authorization".to_owned(), "Bearer sk-test".to_owned())];
        let report = discover(
            Format::Claude,
            &format!("http://{address}"),
            "sk-test",
            Some(&bearer),
            None,
        )
        .await;
        assert_eq!(report.status, DiscoveryStatus::Ok);

        // A compatible gateway (this local address is not api.anthropic.com)
        // defaults to Bearer all by itself now — no retry needed.
        let report = discover(
            Format::Claude,
            &format!("http://{address}"),
            "sk-test",
            None,
            None,
        )
        .await;
        assert_eq!(report.status, DiscoveryStatus::Ok);

        // A pinned `x-api-key` scheme is honored first; when the endpoint
        // rejects it, the read-only probe retries once with Bearer.
        let pinned = vec![
            ("x-api-key".to_owned(), "sk-test".to_owned()),
            ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
        ];
        let report = discover(
            Format::Claude,
            &format!("http://{address}"),
            "sk-test",
            Some(&pinned),
            None,
        )
        .await;
        assert_eq!(report.status, DiscoveryStatus::Ok);

        // A genuinely bad key fails under both schemes.
        let report = discover(
            Format::Claude,
            &format!("http://{address}"),
            "sk-wrong",
            None,
            None,
        )
        .await;
        assert_eq!(report.status, DiscoveryStatus::Error);
        assert_eq!(
            report.error.unwrap().kind,
            DiscoveryErrorKind::InvalidCredentials
        );
    }

    #[tokio::test]
    async fn wrong_credentials_are_reported_without_leaking_the_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/v1/models", get(openai_models));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let report = discover(
            Format::OpenAIChat,
            &format!("http://{address}"),
            "sk-wrong",
            None,
            None,
        )
        .await;
        assert_eq!(report.status, DiscoveryStatus::Error);
        let error = report.error.unwrap();
        assert_eq!(error.kind, DiscoveryErrorKind::InvalidCredentials);
        assert!(!error.message.contains("sk-wrong"));
    }

    #[tokio::test]
    async fn missing_model_endpoint_is_unsupported_not_an_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/v1/models", get(|| async { StatusCode::NOT_FOUND }));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let report = discover(
            Format::OpenAIChat,
            &format!("http://{address}"),
            "sk-test",
            None,
            None,
        )
        .await;
        assert_eq!(report.status, DiscoveryStatus::Unsupported);
        assert_eq!(
            report.error.unwrap().kind,
            DiscoveryErrorKind::EndpointNotFound
        );
    }
}
