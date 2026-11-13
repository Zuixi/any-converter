//! A provider: its endpoints, its keys, the account that signs it, and the
//! plugin that serves it.

use std::collections::HashMap;

use any_converter_core::convert::Format;
use serde::{Deserialize, Serialize};

use super::glob_match;
use super::upstream_api::{UpstreamApi, WireCompat, copilot_api};

/// A provider served by an OpenCode plugin (P6.2).
///
/// The plugin owns the upstream transport: its loader supplies the real
/// base URL/API key, and its model list selects the wire protocol per model.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PluginRef {
    /// npm spec or path, e.g. "@magpie-community/opencode-cursor-auth".
    pub spec: String,
    /// Directory the spec resolves from ("" = process cwd).
    #[serde(default)]
    pub target: String,
}

/// `format` no longer drives the wire (see [`ProviderConfig::effective_api`])
/// and plugin providers do not need it at all, so it gets a serde default
/// instead of being required. `Format` lives in the core crate (out of scope
/// for a `Default` impl), hence this local fallback function.
fn default_format() -> Format {
    Format::OpenAIChat
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    #[serde(default = "default_format")]
    pub format: Format,
    pub base_url: String,
    pub api_key: String,
    /// Optional account id in the secret store (e.g. `gemini:me@example.com`).
    /// When set, requests are signed with that account's OAuth access token
    /// and `api_key` is ignored.
    ///
    /// Single-account convenience form of [`Self::accounts`]: folded in on
    /// read ([`Self::account_ids`]), never merged — setting both is a
    /// config error.
    #[serde(default)]
    pub account: Option<String>,
    /// The account ids this provider tries, in the user's order
    /// (`account_ids` in the secret store, e.g. `gemini:me@example.com`).
    /// When set, requests are signed per account and `account`/`api_key`
    /// are ignored per account. Empty = use [`Self::account`] / `api_key`.
    #[serde(default)]
    pub accounts: Vec<String>,
    /// Explicit upstream API. Old configs keep using the `format` field,
    /// which [`Self::effective_api`] falls back to via
    /// [`UpstreamApi::from_format`]; the `format` field itself is kept for
    /// backward compatibility only and no longer drives URL or conversion
    /// (it remains a historical field for model routing).
    #[serde(default)]
    pub api: Option<UpstreamApi>,
    /// Legacy `wire` override from pre-`UpstreamApi` configs; otherwise
    /// inferred via [`Self::effective_api`].
    #[serde(default)]
    pub wire: Option<WireCompat>,
    #[serde(default)]
    pub model_map: HashMap<String, String>,
    #[serde(default)]
    pub endpoints: ProviderEndpointConfig,
    #[serde(default)]
    pub auth: ProviderAuthConfig,
    /// Optional OpenCode plugin serving this provider's requests (P6.2).
    /// When set, the plugin decides the protocol per model; `api`/`wire`
    /// must stay unset and `base_url`/`api_key` may be empty.
    #[serde(default)]
    pub plugin: Option<PluginRef>,
    /// Egress proxy for this provider's account traffic (sign-in, refresh,
    /// revocation). Wins over [`ServerSettings::proxy`]; `None` falls back
    /// to the server-level proxy (P10, magpie `ProxyOfLogin`).
    #[serde(default)]
    pub proxy: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderEndpointConfig {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub stream_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderAuthConfig {
    #[serde(default)]
    pub scheme: Option<AuthScheme>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    /// Use the provider format's default scheme. Lets a config opt out of
    /// an explicit scheme without deleting the table.
    Auto,
    Bearer,
    ApiKeyHeader,
    XApiKey,
    GoogleApiKey,
    Anthropic,
    None,
}

impl AuthScheme {
    /// The scheme a provider format uses when config has not chosen one and
    /// the endpoint host is unknown. Callers that know the base URL should
    /// prefer [`Self::for_endpoint`].
    pub fn for_format(format: Format) -> AuthScheme {
        match format {
            Format::OpenAIChat | Format::OpenAIResponses => AuthScheme::Bearer,
            Format::Claude => AuthScheme::Anthropic,
            Format::Gemini => AuthScheme::GoogleApiKey,
        }
    }

    /// The scheme an endpoint uses when config has not chosen one.
    ///
    /// Format alone is not enough for Claude: only the official
    /// `api.anthropic.com` signs with `x-api-key` — every compatible gateway
    /// (MiniMax, Cloudflare, enterprise proxies) expects `Authorization:
    /// Bearer` on the same Messages wire.
    pub fn for_endpoint(format: Format, base_url: &str) -> AuthScheme {
        match format {
            Format::Claude if !is_official_anthropic_base_url(base_url) => AuthScheme::Bearer,
            format => AuthScheme::for_format(format),
        }
    }

    /// Resolve `auto` against the endpoint default; other schemes win as-is.
    pub fn resolve(self, format: Format, base_url: &str) -> AuthScheme {
        match self {
            AuthScheme::Auto => AuthScheme::for_endpoint(format, base_url),
            scheme => scheme,
        }
    }

    /// The TOML/JSON spelling of this variant.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthScheme::Auto => "auto",
            AuthScheme::Bearer => "bearer",
            AuthScheme::ApiKeyHeader => "api_key_header",
            AuthScheme::XApiKey => "x_api_key",
            AuthScheme::GoogleApiKey => "google_api_key",
            AuthScheme::Anthropic => "anthropic",
            AuthScheme::None => "none",
        }
    }
}

/// Whether `base_url` is the official first-party Anthropic API.
///
/// Requires the exact origin or a path boundary after it; a bare prefix
/// check would accept lookalikes such as `https://api.anthropic.com.evil`.
/// An empty base URL counts as official: request dispatch falls back to
/// `api.anthropic.com` anyway.
fn is_official_anthropic_base_url(base_url: &str) -> bool {
    const OFFICIAL: &str = "https://api.anthropic.com";
    let trimmed = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    trimmed.is_empty() || trimmed == OFFICIAL || trimmed.starts_with(&format!("{OFFICIAL}/"))
}

impl ProviderConfig {
    /// The account ids in force: `accounts`, else `[account]`, else none.
    /// One source of truth — `account` is folded in on read, never merged.
    /// Empty ids and repeats are dropped so no account is ever tried twice
    /// in one request.
    pub fn account_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        let source: Vec<&str> = if !self.accounts.is_empty() {
            self.accounts.iter().map(String::as_str).collect()
        } else if let Some(account) = &self.account {
            vec![account.as_str()]
        } else {
            Vec::new()
        };
        for id in source {
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    /// Resolve the API used to talk to this provider.
    ///
    /// Precedence: explicit `api` → legacy `wire` key → account prefix
    /// (`gemini`/`antigravity` selects [`UpstreamApi::CodeAssist`]) →
    /// [`UpstreamApi::from_format`] of `format`.
    pub fn effective_api(&self) -> UpstreamApi {
        if let Some(api) = self.api {
            return api;
        }
        if let Some(wire) = self.wire {
            return wire.into();
        }
        // The first account's prefix drives the inference (accounts and
        // the single `account` form fold through account_ids, P9).
        let prefix = self.account_ids().first().and_then(|a| a.split(':').next());
        match prefix {
            Some("gemini" | "antigravity") => UpstreamApi::CodeAssist,
            // Sign in with ChatGPT talks to OpenAI's own API, at /v1/responses.
            Some("chatgpt-api") => UpstreamApi::Responses,
            _ => UpstreamApi::from_format(self.format),
        }
    }

    /// Resolve the API for a specific upstream model.
    ///
    /// Copilot accounts dispatch per model (P7, magpie `copilotAPI` via
    /// [`copilot_api`]); every other provider speaks [`Self::effective_api`].
    pub fn api_for(&self, model: &str) -> UpstreamApi {
        if self.is_copilot_account() {
            copilot_api(model)
        } else {
            self.effective_api()
        }
    }

    /// Whether this provider signs in through a Copilot account
    /// (its first account id starts with `copilot:`; P9).
    pub fn is_copilot_account(&self) -> bool {
        self.account_ids()
            .first()
            .and_then(|account| account.split(':').next())
            == Some("copilot")
    }

    /// The egress proxy in force for this provider: the provider-level
    /// `proxy`, else the server-level one, else none (P10).
    pub fn proxy_for<'a>(&'a self, server_proxy: Option<&'a str>) -> Option<&'a str> {
        self.proxy.as_deref().or(server_proxy)
    }

    /// The base Code Assist generation requests go to for this provider's
    /// accounts: the provider's own `base_url` when set, else the account
    /// prefix's default — `antigravity:` accounts default to the daily host
    /// (magpie `Base`), everything else to the regular host.
    pub fn code_assist_base(&self) -> String {
        if !self.base_url.trim().is_empty() {
            return self.base_url.clone();
        }
        match self
            .account_ids()
            .first()
            .and_then(|id| id.split(':').next())
        {
            Some("antigravity") => ANTIGRAVITY_CODE_ASSIST_BASE.to_string(),
            _ => GEMINI_CODE_ASSIST_BASE.to_string(),
        }
    }

    /// The base a Sign in with ChatGPT account's requests go to: OpenAI's
    /// own API, where the token is good (`siwcAPI`, magpie chatgpt_api.go).
    ///
    /// Kept without the `/v1` the other providers' bases are written without:
    /// the request path already carries it (`/v1/responses`), so a base that
    /// ends in `/v1` — magpie's own `siwcAPI` does — is trimmed here rather
    /// than sending `/v1/v1/responses`.
    pub fn chatgpt_api_base(&self) -> String {
        let base = if self.base_url.trim().is_empty() {
            "https://api.openai.com".to_string()
        } else {
            self.base_url.trim_end_matches('/').to_string()
        };
        base.strip_suffix("/v1").map(str::to_string).unwrap_or(base)
    }

    /// The base `loadCodeAssist` project/flags discovery goes to for this
    /// provider's accounts: the provider's own `base_url` when set, else
    /// the regular Code Assist host — Antigravity accounts discover on the
    /// regular host even though generation goes to the daily one (magpie
    /// `LoadBase`).
    pub fn code_assist_load_base(&self) -> String {
        if !self.base_url.trim().is_empty() {
            return self.base_url.clone();
        }
        GEMINI_CODE_ASSIST_BASE.to_string()
    }

    /// Resolve a client model name to the upstream model using model_map.
    ///
    /// Priority: exact match → longest prefix wildcard → `"*"` → passthrough.
    pub fn resolve_model(&self, model: &str) -> String {
        if let Some(mapped) = self.model_map.get(model) {
            return mapped.clone();
        }
        // Longest prefix wildcard match (e.g. "claude-opus-*" beats "claude-*")
        let mut best_match: Option<(&str, &str)> = None;
        for (pattern, target) in &self.model_map {
            if pattern == "*" {
                continue;
            }
            if glob_match(pattern, model) && best_match.is_none_or(|(p, _)| pattern.len() > p.len())
            {
                best_match = Some((pattern.as_str(), target.as_str()));
            }
        }
        if let Some((_, target)) = best_match {
            return target.to_string();
        }
        if let Some(fallback) = self.model_map.get("*") {
            return fallback.clone();
        }
        model.to_string()
    }
}

/// The Code Assist host a Gemini account's requests go to.
/// Default Code Assist request base for Gemini CLI accounts.
pub const GEMINI_CODE_ASSIST_BASE: &str = "https://cloudcode-pa.googleapis.com";

/// Default Code Assist request base for Antigravity accounts: generation
/// traffic is served on the `daily-` host (magpie `Base`), while
/// `loadCodeAssist` discovery stays on the regular host (magpie
/// `LoadBase`).
pub const ANTIGRAVITY_CODE_ASSIST_BASE: &str = "https://daily-cloudcode-pa.googleapis.com";

#[cfg(test)]
mod auth_scheme_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::config::AuthScheme;

    #[test]
    fn claude_defaults_follow_the_endpoint_host() {
        // Official Anthropic signs with x-api-key…
        assert_eq!(
            AuthScheme::for_endpoint(Format::Claude, "https://api.anthropic.com"),
            AuthScheme::Anthropic
        );
        assert_eq!(
            AuthScheme::for_endpoint(Format::Claude, "https://api.anthropic.com/"),
            AuthScheme::Anthropic
        );
        // …but every compatible gateway expects Bearer on the same wire.
        assert_eq!(
            AuthScheme::for_endpoint(Format::Claude, "https://api.minimax.cn/anthropic"),
            AuthScheme::Bearer
        );
        assert_eq!(
            AuthScheme::for_endpoint(Format::Claude, "http://127.0.0.1:8082"),
            AuthScheme::Bearer
        );
        // A lookalike host must not count as official.
        assert_eq!(
            AuthScheme::for_endpoint(Format::Claude, "https://api.anthropic.com.evil.com"),
            AuthScheme::Bearer
        );
    }

    #[test]
    fn resolve_honors_explicit_schemes_and_other_formats() {
        let minimax = "https://api.minimax.cn/anthropic";
        assert_eq!(
            AuthScheme::Auto.resolve(Format::Claude, minimax),
            AuthScheme::Bearer
        );
        // An explicit scheme always wins over the endpoint default.
        assert_eq!(
            AuthScheme::Anthropic.resolve(Format::Claude, minimax),
            AuthScheme::Anthropic
        );
        assert_eq!(
            AuthScheme::Auto.resolve(Format::Gemini, minimax),
            AuthScheme::GoogleApiKey
        );
        assert_eq!(
            AuthScheme::Auto.resolve(Format::OpenAIChat, minimax),
            AuthScheme::Bearer
        );
    }
}
