use std::collections::HashMap;
use std::path::Path;
#[cfg(unix)]
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use serde::Deserialize;

pub mod logging;
pub mod provider;
pub mod routes;
pub mod upstream_api;

pub use logging::{ConsoleConfig, LogFileConfig, LoggingConfig, RequestLogConfig};
pub use provider::{
    ANTIGRAVITY_CODE_ASSIST_BASE, AuthScheme, GEMINI_CODE_ASSIST_BASE, PluginRef,
    ProviderAuthConfig, ProviderConfig, ProviderEndpointConfig,
};
pub use routes::{ModelMetadata, ModelRouteConfig, ResolvedRoute, RouteConfig, RouteStrategy};
pub use upstream_api::{UpstreamApi, WireCompat, copilot_api};

use logging::{
    default_console_format, default_console_output, default_false, default_log_level,
    default_max_capture_bytes, default_max_disk_mb, default_trace_preview_bytes, default_true,
};

/// Warn (once per path) when a config that carries API keys is readable by
/// group or others: `api_key` is plaintext in TOML, and 0644 hands it to
/// every local user. Secrets in `secrets.json` are already 0600.
#[cfg(unix)]
pub fn warn_if_world_readable(path: &Path) {
    let Some(message) = world_readable_warning(path) else {
        return;
    };

    static WARNED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut warned = warned.lock().unwrap_or_else(|error| error.into_inner());
    if !warned.insert(path.to_path_buf()) {
        return;
    }
    drop(warned);

    log::warn!("{message}");
}

/// No-op on platforms where POSIX permission bits do not apply (Windows ACL
/// hardening is out of scope here).
#[cfg(not(unix))]
pub fn warn_if_world_readable(_path: &Path) {}

/// Build the warning for `path` when it is group/world readable and carries a
/// plaintext provider key; `None` means there is nothing to warn about.
#[cfg(unix)]
fn world_readable_warning(path: &Path) -> Option<String> {
    if !is_world_readable(path) {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    let config = ServerConfig::from_toml(&content).ok()?;
    if !config.providers.iter().any(|p| !p.api_key.is_empty()) {
        return None;
    }
    let mode = config_mode(path)?;
    Some(format!(
        "config {} is readable by group/others (mode 0{mode:03o}), so plaintext provider api_key values can be read by other local users; run `chmod 600 {}` to fix it, and consider moving credentials to `any-converter accounts add` OAuth credentials",
        path.display(),
        path.display(),
    ))
}

/// Whether group/other permission bits are set on `path`.
#[cfg(unix)]
fn is_world_readable(path: &Path) -> bool {
    config_mode(path).is_some_and(|mode| mode & 0o077 != 0)
}

#[cfg(unix)]
fn config_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServerConfig {
    pub server: ServerSettings,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub model_routes: Vec<ModelRouteConfig>,
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
    #[serde(default)]
    pub model_metadata: HashMap<String, ModelMetadata>,
    /// `[pricing."model-prefix"]` → USD per million tokens.
    #[serde(default)]
    pub pricing: crate::pricing::PriceTable,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PassThroughMode {
    #[default]
    Allow,
    Deny,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PassThroughHeadersConfig {
    /// `allow` = forward all non-blacklisted client headers (default).
    /// `deny`  = only forward headers explicitly allowed by `allow`.
    #[serde(default)]
    pub mode: PassThroughMode,

    /// Additional header name glob patterns to allow.
    /// In `allow` mode this extends the default passthrough set.
    /// In `deny` mode only headers matching these patterns are forwarded.
    #[serde(default)]
    pub allow: Vec<String>,

    /// Header name glob patterns to drop.
    /// In `allow` mode this blocks headers that would otherwise be forwarded.
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub api_key: Option<String>,
    #[serde(default)]
    pub pass_through_headers: PassThroughHeadersConfig,
    /// Global egress proxy (e.g. `http://127.0.0.1:7890`): sign-in, token
    /// refresh and revocation clients leave through it (magpie
    /// `ProxyOfLogin` — a sign-in must use the same exit as the account's
    /// own requests). Provider-level `proxy` wins over this one.
    #[serde(default)]
    pub proxy: Option<String>,
}

/// Build a reqwest client routed through `proxy` when one is configured.
///
/// Shared by the sign-in clients, the account-refresh client and the
/// revocation client so login/refresh/revoke always leave through the same
/// proxy the account's own requests use. An explicit proxy disables the
/// ambient environment proxies so the two never stack.
pub fn build_proxied_client(proxy: Option<&str>) -> Result<reqwest::Client, reqwest::Error> {
    match proxy {
        Some(url) => reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(url)?)
            .build(),
        None => Ok(reqwest::Client::new()),
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            dir: None,
            max_disk_mb: default_max_disk_mb(),
            conversion_log: true,
            upstream_headers_log: false,
            console: ConsoleConfig::default(),
            files: Vec::new(),
            request_log: RequestLogConfig::default(),
            retention_days: logging::default_retention_days(),
            body_retention_days: logging::default_body_retention_days(),
            rollup_retention_days: 0,
            jsonl_mirror: false,
        }
    }
}

impl Default for RequestLogConfig {
    fn default() -> Self {
        Self {
            enabled: default_false(),
            max_capture_bytes: default_max_capture_bytes(),
            trace_enabled: default_true(),
            trace_max_preview_bytes: default_trace_preview_bytes(),
        }
    }
}

impl Default for ConsoleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            output: default_console_output(),
            level: None,
            format: default_console_format(),
        }
    }
}

/// Default gateway port, shared by the server, CLI, and Desktop defaults.
///
/// Keep every default surface pointing here instead of repeating the literal.
pub const DEFAULT_PORT: u16 = 3028;

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl ServerConfig {
    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(content)
    }

    /// Resolve a provider for the given model name via `[[model_routes]]` globs.
    ///
    /// Legacy `[[routes]]` are ignored (see the CLI startup warning).
    ///
    /// Agent wiring exposes models as `<provider>/<model>` (so several
    /// providers can share a model name without ambiguity), so a miss on
    /// the full string is retried with the `provider/` prefix stripped: the
    /// route tables name bare models (`"deepseek-chat"`, `"deepseek-*"`),
    /// and the upstream model is the bare name either way.
    pub fn resolve_provider(&self, client_model: &str) -> Option<ResolvedRoute> {
        // Agent wiring exposes models as `<provider>/<model>`, so a bare-model
        // route table must also answer the prefixed form. A specific (glob-
        // free) pattern is tried first on both forms; the prefixed form is
        // tried before any wildcard so a catch-all `"*"` cannot shadow a
        // named route, mirroring how model_map matches specific before glob.
        if let Some(resolved) = self.resolve_provider_form(client_model, false) {
            return Some(resolved);
        }
        if let Some((provider_prefix, bare_model)) = client_model.split_once('/') {
            // Only strip when the prefix names a configured provider: a model
            // id that merely contains a slash (some vendors use dated ids)
            // must keep its full name for matching.
            if self.find_provider(provider_prefix).is_some() {
                if let Some(resolved) = self.resolve_provider_form(bare_model, false) {
                    return Some(Self::rebase_upstream(resolved, client_model, bare_model));
                }
            }
        }
        if let Some(resolved) = self.resolve_provider_form(client_model, true) {
            // A bare-model wildcard match (with a known provider prefix) is
            // more specific than the same string hitting a catch-all, so try
            // the bare form first inside the wildcard pass too.
            if let Some((prefix, bare)) = client_model.split_once('/') {
                if self.find_provider(prefix).is_some() {
                    if let Some(bare_resolved) = self.resolve_provider_form(bare, true) {
                        return Some(Self::rebase_upstream(bare_resolved, client_model, bare));
                    }
                }
            }
            return Some(resolved);
        }
        if let Some((_, bare)) = client_model.split_once('/') {
            return self.resolve_provider_form(bare, true);
        }
        None
    }

    /// Applies `allow_wildcards` as a filter over route patterns so callers
    /// can prefer specific matches before falling back to globs.
    fn resolve_provider_form(
        &self,
        client_model: &str,
        allow_wildcards: bool,
    ) -> Option<ResolvedRoute> {
        if !self.model_routes.is_empty() {
            for mr in &self.model_routes {
                let is_wildcard = mr.pattern.contains('*') || mr.pattern.contains('?');
                if is_wildcard && !allow_wildcards {
                    continue;
                }
                if glob_match(&mr.pattern, client_model) {
                    let names = mr.provider_names();
                    if names.is_empty() {
                        continue;
                    }
                    let upstream = mr.upstream_model.clone().unwrap_or_else(|| {
                        if let Some(provider) = self.find_provider(&names[0]) {
                            provider.resolve_model(client_model)
                        } else {
                            client_model.to_string()
                        }
                    });
                    return Some(ResolvedRoute {
                        provider_names: names,
                        upstream_model: upstream,
                        strategy: mr.strategy.clone(),
                    });
                }
            }
        }
        None
    }

    /// Rewrites the resolved upstream model when a bare-model match kept the
    /// catalog id as the passthrough name: the upstream never wants the
    /// `provider/` prefix.
    fn rebase_upstream(
        resolved: ResolvedRoute,
        client_model: &str,
        bare_model: &str,
    ) -> ResolvedRoute {
        if resolved.upstream_model == client_model {
            ResolvedRoute {
                provider_names: resolved.provider_names,
                upstream_model: bare_model.to_string(),
                strategy: resolved.strategy,
            }
        } else {
            resolved
        }
    }
    pub fn find_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// Validate config references at startup. Returns a list of warnings/errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let provider_names: Vec<&str> = self.providers.iter().map(|p| p.name.as_str()).collect();

        for (i, provider) in self.providers.iter().enumerate() {
            // Plugin providers may leave `base_url`/`api_key` empty (the
            // plugin's loader supplies them), but the plugin decides the
            // protocol per model, so an explicit `api`/`wire` is a conflict.
            if provider.plugin.is_some() && (provider.api.is_some() || provider.wire.is_some()) {
                errors.push(format!(
                    "providers[{i}] (name=\"{}\"): `api`/`wire` must not be set together with `plugin` (the plugin decides the protocol per model)",
                    provider.name
                ));
            }
            // `account` and `accounts` together are ambiguous: the single-
            // account field is a convenience form of the list, never merged.
            if provider.account.is_some() && !provider.accounts.is_empty() {
                errors.push(format!(
                    "providers[{i}] (name=\"{}\"): `account` and `accounts` cannot both be set — `account` is the single-account form of `accounts`; keep one",
                    provider.name
                ));
            }
        }

        for (i, mr) in self.model_routes.iter().enumerate() {
            for name in mr.provider_names() {
                if !provider_names.contains(&name.as_str()) {
                    errors.push(format!(
                        "model_routes[{}] (pattern=\"{}\"): provider \"{}\" not found",
                        i, mr.pattern, name
                    ));
                }
            }
            if mr.provider_names().is_empty() {
                errors.push(format!(
                    "model_routes[{}] (pattern=\"{}\"): no provider specified",
                    i, mr.pattern
                ));
            }
        }
        errors
    }

    /// Collect all model names available for the /v1/models endpoint.
    pub fn available_models(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut models = Vec::new();
        if !self.model_routes.is_empty() {
            for mr in &self.model_routes {
                if mr.pattern != "*" && !mr.pattern.contains('*') && seen.insert(mr.pattern.clone())
                {
                    models.push(mr.pattern.clone());
                }
            }
        }
        for provider in &self.providers {
            for model in provider.model_map.keys() {
                if model != "*" && seen.insert(model.clone()) {
                    models.push(model.clone());
                }
            }
        }
        models
    }
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        }
    }
}

/// `$ANY_CONVERTER_SECRETS` overrides the store location; otherwise it is
/// `<home>/secrets.json`, where the home is resolved by
/// [`crate::home::Home`]. `ANY_CONVERTER_HOME` may override the home.
///
/// Shared by the server runtime and the CLI so both agree on the account
/// store location.
pub fn secrets_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os(crate::home::SECRETS_ENV)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
    {
        return path;
    }
    crate::home::Home::discover()
        .map(|home| home.secrets_path().to_path_buf())
        .unwrap_or_else(|_| config_dir().join("secrets.json"))
}

fn config_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home)
        .join(".config")
        .join("any-converter")
}

/// Simple glob matching: supports `*` as a wildcard segment.
///
/// - `"*"` matches everything
/// - `"claude-*"` matches any string starting with `"claude-"`
/// - `"*-turbo"` matches any string ending with `"-turbo"`
/// - `"gpt-4.1"` matches exactly `"gpt-4.1"`
pub(crate) fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 2 {
        let (prefix, suffix) = (parts[0], parts[1]);
        return value.starts_with(prefix)
            && value.ends_with(suffix)
            && value.len() >= prefix.len() + suffix.len();
    }
    // Multi-wildcard: greedy sequential match
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !value.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            if !value[pos..].ends_with(part) {
                return false;
            }
        } else if let Some(found) = value[pos..].find(part) {
            pos += found + part.len();
        } else {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use any_converter_core::convert::Format;

    fn sample_provider(model_map: HashMap<String, String>) -> ProviderConfig {
        ProviderConfig {
            name: "openai".into(),
            format: Format::OpenAIChat,
            base_url: "https://api.openai.com".into(),
            api_key: "sk-test".into(),
            account: None,
            accounts: Vec::new(),
            api: None,
            wire: None,
            model_map,
            endpoints: Default::default(),
            auth: Default::default(),
            plugin: None,
            proxy: None,
        }
    }

    #[test]
    fn test_parse_config() {
        let toml_str = r#"
[server]
host = "0.0.0.0"
port = 9090
api_key = "sk-test"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-proj-xxx"

[providers.model_map]
"*" = "gpt-4.1"

[[routes]]
client_format = "claude"
provider = "openai"
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 9090);
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.providers[0].name, "openai");
        assert_eq!(config.routes.len(), 1);
        assert_eq!(config.routes[0].client_format, Format::Claude);
    }

    #[test]
    fn test_model_map_exact_match() {
        let mut map = HashMap::new();
        map.insert("claude-sonnet-4".into(), "gpt-4.1".into());
        let provider = sample_provider(map);
        assert_eq!(provider.resolve_model("claude-sonnet-4"), "gpt-4.1");
    }

    #[test]
    fn test_model_map_wildcard() {
        let mut map = HashMap::new();
        map.insert("*".into(), "gpt-4.1".into());
        let provider = sample_provider(map);
        assert_eq!(provider.resolve_model("unknown-model"), "gpt-4.1");
    }

    #[test]
    fn test_model_map_passthrough() {
        let provider = sample_provider(HashMap::new());
        assert_eq!(provider.resolve_model("claude-sonnet-4"), "claude-sonnet-4");
    }

    #[test]
    fn test_model_map_longest_prefix_wins() {
        let mut map = HashMap::new();
        map.insert("claude-*".into(), "claude-generic".into());
        map.insert("claude-opus-*".into(), "claude-opus-latest".into());
        map.insert("*".into(), "fallback".into());
        let provider = sample_provider(map);
        assert_eq!(
            provider.resolve_model("claude-opus-4"),
            "claude-opus-latest"
        );
        assert_eq!(provider.resolve_model("claude-sonnet-4"), "claude-generic");
        assert_eq!(provider.resolve_model("gpt-4.1"), "fallback");
    }

    #[test]
    fn test_logging_config_defaults() {
        let config = LoggingConfig::default();
        assert_eq!(config.level, "info");
        assert!(config.dir.is_none());
        assert_eq!(config.max_disk_mb, 500);
        assert!(config.conversion_log);
        assert!(config.console.enabled);
        assert_eq!(config.console.output, "stdout");
        assert!(config.console.level.is_none());
        assert_eq!(config.console.format, "pretty");
        assert!(config.files.is_empty());
        assert!(!config.request_log.enabled);
        assert_eq!(config.request_log.max_capture_bytes, 1024 * 1024);
        assert!(config.request_log.trace_enabled);
        assert_eq!(config.request_log.trace_max_preview_bytes, 2048);
        assert_eq!(config.retention_days, 30);
        assert_eq!(config.body_retention_days, 7);
        assert_eq!(config.rollup_retention_days, 0);
        assert!(!config.jsonl_mirror);
    }

    #[test]
    fn test_retention_defaults_when_keys_absent() {
        let toml_str = r#"
[server]
api_key = "sk-test"

[logging]
dir = "./logs"
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        assert_eq!(config.logging.retention_days, 30);
        assert_eq!(config.logging.body_retention_days, 7);
        assert_eq!(config.logging.rollup_retention_days, 0);
    }

    #[test]
    fn test_jsonl_mirror_defaults_to_false_when_key_absent() {
        let toml_str = r#"
[server]
api_key = "sk-test"

[logging]
level = "info"
dir = "./logs"
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        assert!(!config.logging.jsonl_mirror);
    }

    #[test]
    fn test_jsonl_mirror_parses_true() {
        let toml_str = r#"
[server]
api_key = "sk-test"

[logging]
dir = "./logs"
jsonl_mirror = true
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        assert!(config.logging.jsonl_mirror);
    }

    #[test]
    fn test_parse_logging_config_with_console_and_files() {
        let toml_str = r#"
[server]
host = "0.0.0.0"
port = 8080

[logging]
level = "debug"
dir = "./logs"
max_disk_mb = 100
conversion_log = false

[logging.console]
enabled = true
output = "stderr"
level = "warn"
format = "json"

[[logging.files]]
name = "general"
level = "info"
format = "json"
rotation = "daily"

[[logging.files]]
name = "errors"
level = "error"
target = "my_target"
format = "pretty"
rotation = "never"
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        assert_eq!(config.logging.level, "debug");
        assert_eq!(config.logging.dir.as_deref(), Some("./logs"));
        assert_eq!(config.logging.max_disk_mb, 100);
        assert!(!config.logging.conversion_log);

        assert!(config.logging.console.enabled);
        assert_eq!(config.logging.console.output, "stderr");
        assert_eq!(config.logging.console.level.as_deref(), Some("warn"));
        assert_eq!(config.logging.console.format, "json");

        assert_eq!(config.logging.files.len(), 2);
        assert_eq!(config.logging.files[0].name, "general");
        assert_eq!(config.logging.files[0].level.as_deref(), Some("info"));
        assert_eq!(config.logging.files[0].format, "json");
        assert_eq!(config.logging.files[0].rotation, "daily");
        assert!(config.logging.files[0].target.is_none());

        assert_eq!(config.logging.files[1].name, "errors");
        assert_eq!(config.logging.files[1].target.as_deref(), Some("my_target"));
        assert_eq!(config.logging.files[1].format, "pretty");
        assert_eq!(config.logging.files[1].rotation, "never");
    }

    #[test]
    fn test_effective_api_precedence() {
        let mut provider = sample_provider(HashMap::new());
        // No api/wire/account: falls back to the historical format field.
        assert_eq!(provider.effective_api(), UpstreamApi::Chat);

        provider.account = Some("gemini:me@example.com".into());
        assert_eq!(provider.effective_api(), UpstreamApi::CodeAssist);

        provider.account = Some("antigravity:me@example.com".into());
        assert_eq!(provider.effective_api(), UpstreamApi::CodeAssist);

        provider.account = Some("copilot:me".into());
        assert_eq!(provider.effective_api(), UpstreamApi::Chat);

        // Legacy wire wins over account inference.
        provider.wire = Some(WireCompat::GeminiApi);
        provider.account = Some("gemini:me@example.com".into());
        assert_eq!(provider.effective_api(), UpstreamApi::Gemini);

        provider.wire = Some(WireCompat::CodeAssist);
        provider.account = None;
        assert_eq!(provider.effective_api(), UpstreamApi::CodeAssist);

        // Explicit api beats legacy wire, account, and format.
        provider.api = Some(UpstreamApi::Responses);
        provider.wire = Some(WireCompat::CodeAssist);
        provider.account = Some("gemini:me@example.com".into());
        assert_eq!(provider.effective_api(), UpstreamApi::Responses);
    }

    #[test]
    fn test_code_assist_base_defaults_and_override() {
        // The provider's own base_url always wins, for both bases.
        let mut provider = sample_provider(HashMap::new());
        provider.account = Some("antigravity:me@example.com".into());
        provider.base_url = "https://my-host.example.test".into();
        assert_eq!(provider.code_assist_base(), "https://my-host.example.test");
        assert_eq!(
            provider.code_assist_load_base(),
            "https://my-host.example.test"
        );

        // Antigravity accounts default generation to the daily host; the
        // loadCodeAssist discovery base stays on the regular host (magpie
        // `Base` / `LoadBase`).
        provider.base_url = String::new();
        provider.account = Some("antigravity:me@example.com".into());
        assert_eq!(
            provider.code_assist_base(),
            "https://daily-cloudcode-pa.googleapis.com"
        );
        assert_eq!(
            provider.code_assist_load_base(),
            "https://cloudcode-pa.googleapis.com"
        );

        // The `accounts` list form folds through account_ids like the
        // single-account field.
        provider.account = None;
        provider.accounts = vec!["antigravity:me@example.com".into()];
        assert_eq!(
            provider.code_assist_base(),
            "https://daily-cloudcode-pa.googleapis.com"
        );

        // Gemini (and everything else) defaults both bases to the regular
        // host.
        provider.accounts = vec!["gemini:me@example.com".into()];
        assert_eq!(
            provider.code_assist_base(),
            "https://cloudcode-pa.googleapis.com"
        );
        assert_eq!(
            provider.code_assist_load_base(),
            "https://cloudcode-pa.googleapis.com"
        );
        provider.accounts.clear();
        assert_eq!(
            provider.code_assist_base(),
            "https://cloudcode-pa.googleapis.com"
        );
    }

    #[test]
    fn test_api_from_format() {
        // The four formats round-trip through UpstreamApi::to_format.
        for (format, api) in [
            (Format::OpenAIChat, UpstreamApi::Chat),
            (Format::Claude, UpstreamApi::Claude),
            (Format::OpenAIResponses, UpstreamApi::Responses),
            (Format::Gemini, UpstreamApi::Gemini),
        ] {
            assert_eq!(UpstreamApi::from_format(format), api);
            assert_eq!(api.to_format(), format);
        }
        // CodeAssist is not a Format; its wire format is Gemini.
        assert_eq!(UpstreamApi::CodeAssist.to_format(), Format::Gemini);
    }

    #[test]
    fn test_from_npm_maps_ai_sdk_packages() {
        for (npm, expected) in [
            ("@ai-sdk/anthropic", UpstreamApi::Claude),
            ("@ai-sdk/google-vertex/anthropic", UpstreamApi::Claude),
            ("@ai-sdk/openai", UpstreamApi::Responses),
            ("@ai-sdk/azure", UpstreamApi::Responses),
            ("@ai-sdk/google", UpstreamApi::CodeAssist),
            // Copilot's per-model dispatch lands in P7.
            ("@ai-sdk/github-copilot", UpstreamApi::Chat),
            ("@ai-sdk/openai-compatible", UpstreamApi::Chat),
            ("@ai-sdk/unknown", UpstreamApi::Chat),
            ("", UpstreamApi::Chat),
        ] {
            assert_eq!(UpstreamApi::from_npm(npm), expected, "npm = {npm:?}");
        }
    }

    #[test]
    fn test_legacy_wire_key_still_parses() {
        for (wire_value, expected) in [
            ("code_assist", UpstreamApi::CodeAssist),
            ("gemini_api", UpstreamApi::Gemini),
        ] {
            let toml_str = format!(
                r#"
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "legacy"
format = "gemini"
base_url = "https://cloudcode-pa.googleapis.com"
api_key = ""
account = "gemini:me@example.com"
wire = "{wire_value}"
"#
            );
            let config = ServerConfig::from_toml(&toml_str).unwrap();
            let provider = &config.providers[0];
            assert_eq!(provider.effective_api(), expected, "wire = {wire_value:?}");
        }
    }

    #[test]
    fn test_api_is_optional_and_defaults_from_format() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-proj-xxx"
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        let provider = &config.providers[0];
        assert_eq!(provider.api, None);
        assert_eq!(provider.wire, None);
        assert_eq!(provider.account, None);
        assert_eq!(provider.effective_api(), UpstreamApi::Chat);
    }

    #[test]
    fn test_account_ids_folds_single_account_and_dedupes() {
        // Single-account convenience form folds into the list on read.
        let mut provider = sample_provider(HashMap::new());
        assert!(provider.account_ids().is_empty());
        provider.account = Some("gemini:me@example.com".into());
        assert_eq!(provider.account_ids(), vec!["gemini:me@example.com"]);

        // `accounts` wins outright; `account` is never merged.
        provider.accounts = vec!["gemini:a@x".into(), "gemini:b@x".into()];
        assert_eq!(provider.account_ids(), vec!["gemini:a@x", "gemini:b@x"]);

        // Empty ids and repeats are dropped: one request never tries the
        // same account twice.
        provider.accounts = vec![
            "gemini:a@x".into(),
            String::new(),
            "gemini:a@x".into(),
            "gemini:c@x".into(),
        ];
        assert_eq!(provider.account_ids(), vec!["gemini:a@x", "gemini:c@x"]);
    }

    #[test]
    fn test_account_ids_parse_from_toml() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "gemini"
format = "gemini"
base_url = "https://cloudcode-pa.googleapis.com"
api_key = ""
accounts = ["gemini:a@x", "gemini:b@x"]
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        let provider = &config.providers[0];
        assert_eq!(provider.account, None);
        assert_eq!(provider.account_ids(), vec!["gemini:a@x", "gemini:b@x"],);
    }

    #[test]
    fn test_validate_rejects_account_and_accounts_together() {
        let mut provider = sample_provider(HashMap::new());
        provider.account = Some("gemini:me@example.com".into());
        provider.accounts = vec!["gemini:a@x".into()];
        let config = ServerConfig {
            pricing: Default::default(),
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![provider],
            model_routes: vec![],
            routes: vec![],
            model_metadata: HashMap::new(),
            logging: LoggingConfig::default(),
        };
        let errors = config.validate();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("`account` and `accounts` cannot both be set"),
            "{}",
            errors[0]
        );
    }

    #[test]
    fn test_parse_provider_account_and_wire() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "code-assist"
format = "gemini"
base_url = "https://cloudcode-pa.googleapis.com"
api_key = ""
account = "gemini:me@example.com"
wire = "code_assist"
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        let provider = &config.providers[0];
        assert_eq!(provider.account.as_deref(), Some("gemini:me@example.com"));
        assert_eq!(provider.wire, Some(WireCompat::CodeAssist));
        assert_eq!(provider.effective_api(), UpstreamApi::CodeAssist);
    }

    #[test]
    fn test_copilot_api_per_model_dispatch() {
        for (model, expected) in [
            ("gpt-5", UpstreamApi::Responses),
            ("gpt-5.6-luna", UpstreamApi::Responses),
            ("gpt-50", UpstreamApi::Responses),
            ("gpt-5-mini", UpstreamApi::Chat),
            // magpie's check is a HasPrefix on the whole model name.
            ("gpt-5-mini-2", UpstreamApi::Chat),
            ("gpt-4.1", UpstreamApi::Chat),
            ("claude-sonnet-4-5", UpstreamApi::Chat),
            ("gpt-", UpstreamApi::Chat),
            ("claude-opus-4-1", UpstreamApi::Chat),
            ("gemini-pro", UpstreamApi::Chat),
        ] {
            assert_eq!(copilot_api(model), expected, "model = {model:?}");
        }
    }

    #[test]
    fn test_api_for_copilot_account_dispatches_per_model() {
        let mut provider = sample_provider(HashMap::new());
        assert!(!provider.is_copilot_account());
        assert_eq!(provider.api_for("gpt-5"), UpstreamApi::Chat);

        provider.account = Some("copilot:octocat".into());
        assert!(provider.is_copilot_account());
        assert_eq!(provider.api_for("gpt-5.6-luna"), UpstreamApi::Responses);
        assert_eq!(provider.api_for("claude-sonnet-4-5"), UpstreamApi::Chat);

        // Non-Copilot accounts keep effective_api() regardless of model.
        provider.account = Some("gemini:me@example.com".into());
        assert!(!provider.is_copilot_account());
        assert_eq!(provider.api_for("gpt-5"), UpstreamApi::CodeAssist);
        provider.account = None;
        assert!(!provider.is_copilot_account());
        assert_eq!(provider.api_for("gpt-5"), provider.effective_api());
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("claude-*", "claude-sonnet-4"));
        assert!(glob_match("claude-*", "claude-opus-4"));
        assert!(!glob_match("claude-*", "gpt-4.1"));
        assert!(glob_match("*-turbo", "gpt-4-turbo"));
        assert!(!glob_match("*-turbo", "gpt-4"));
        assert!(glob_match("gpt-4.1", "gpt-4.1"));
        assert!(!glob_match("gpt-4.1", "gpt-4.2"));
        assert!(glob_match("gpt-*-turbo", "gpt-4-turbo"));
        assert!(!glob_match("gpt-*-turbo", "gpt-4-mini"));
    }

    #[test]
    fn test_parse_model_routes() {
        let toml_str = r#"
[server]
host = "0.0.0.0"
port = 8080

[[providers]]
name = "anthropic"
format = "claude"
base_url = "https://api.anthropic.com"
api_key = "sk-ant-xxx"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-proj-xxx"

[[model_routes]]
pattern = "claude-*"
provider = "anthropic"

[[model_routes]]
pattern = "gpt-*"
provider = "openai"

[[model_routes]]
pattern = "*"
provider = "openai"
upstream_model = "gpt-4.1-mini"
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        assert_eq!(config.model_routes.len(), 3);
        assert_eq!(config.model_routes[0].pattern, "claude-*");
        assert_eq!(
            config.model_routes[0].provider.as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            config.model_routes[2].upstream_model.as_deref(),
            Some("gpt-4.1-mini")
        );
    }

    #[test]
    fn test_parse_provider_endpoint_and_auth_overrides() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "azure"
format = "openai_chat"
base_url = "https://example.openai.azure.com/openai/deployments/my-deployment"
api_key = "secret"

[providers.endpoints]
path = "/chat/completions?api-version=2024-10-21"

[providers.auth]
scheme = "api_key_header"
headers = { "x-ms-client-request-id" = "test-client" }
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        let provider = &config.providers[0];

        assert_eq!(
            provider.endpoints.path.as_deref(),
            Some("/chat/completions?api-version=2024-10-21")
        );
        assert_eq!(provider.auth.scheme, Some(AuthScheme::ApiKeyHeader));
        assert_eq!(
            provider.auth.headers.get("x-ms-client-request-id"),
            Some(&"test-client".to_string())
        );
    }

    #[test]
    fn test_resolve_provider_model_routes() {
        let config = ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![
                ProviderConfig {
                    name: "anthropic".into(),
                    format: Format::Claude,
                    base_url: "https://api.anthropic.com".into(),
                    api_key: "sk-ant".into(),
                    account: None,
                    accounts: Vec::new(),
                    api: None,
                    wire: None,
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                    plugin: None,
                    proxy: None,
                },
                ProviderConfig {
                    name: "openai".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://api.openai.com".into(),
                    api_key: "sk-proj".into(),
                    account: None,
                    accounts: Vec::new(),
                    api: None,
                    wire: None,
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                    plugin: None,
                    proxy: None,
                },
            ],
            model_routes: vec![
                ModelRouteConfig {
                    pattern: "claude-*".into(),
                    provider: Some("anthropic".into()),
                    providers: vec![],
                    upstream_model: None,
                    strategy: RouteStrategy::Priority,
                },
                ModelRouteConfig {
                    pattern: "gpt-*".into(),
                    provider: Some("openai".into()),
                    providers: vec![],
                    upstream_model: None,
                    strategy: RouteStrategy::Priority,
                },
                ModelRouteConfig {
                    pattern: "*".into(),
                    provider: Some("openai".into()),
                    providers: vec![],
                    upstream_model: Some("gpt-4.1-mini".into()),
                    strategy: RouteStrategy::Priority,
                },
            ],
            routes: vec![],
            model_metadata: HashMap::new(),
            pricing: Default::default(),
            logging: LoggingConfig::default(),
        };

        let r = config.resolve_provider("claude-sonnet-4").unwrap();
        assert_eq!(r.provider_names, vec!["anthropic"]);
        assert_eq!(r.upstream_model, "claude-sonnet-4");

        let r = config.resolve_provider("gpt-4.1").unwrap();
        assert_eq!(r.provider_names, vec!["openai"]);
        assert_eq!(r.upstream_model, "gpt-4.1");

        // Wildcard fallback with upstream_model override
        let r = config.resolve_provider("unknown-model").unwrap();
        assert_eq!(r.provider_names, vec!["openai"]);
        assert_eq!(r.upstream_model, "gpt-4.1-mini");
    }

    #[test]
    fn test_legacy_routes_ignored() {
        let provider = ProviderConfig {
            name: "openai".into(),
            format: Format::OpenAIChat,
            base_url: "https://api.openai.com".into(),
            api_key: "sk-proj".into(),
            account: None,
            accounts: Vec::new(),
            api: None,
            wire: None,
            model_map: [("*".into(), "gpt-4.1".into())].into(),
            endpoints: Default::default(),
            auth: Default::default(),
            plugin: None,
            proxy: None,
        };
        let server = ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
            proxy: None,
        };

        // Both present: model_routes wins.
        let config = ServerConfig {
            server: server.clone(),
            providers: vec![provider.clone()],
            model_routes: vec![ModelRouteConfig {
                pattern: "claude-*".into(),
                provider: Some("openai".into()),
                providers: vec![],
                upstream_model: None,
                strategy: RouteStrategy::Priority,
            }],
            routes: vec![RouteConfig {
                client_format: Format::Claude,
                provider: "openai".into(),
            }],
            model_metadata: HashMap::new(),
            pricing: Default::default(),
            logging: LoggingConfig::default(),
        };
        let r = config.resolve_provider("claude-sonnet-4").unwrap();
        assert_eq!(r.provider_names, vec!["openai"]);
        assert_eq!(r.upstream_model, "gpt-4.1");

        // Only legacy routes: no resolution at all.
        let config = ServerConfig {
            model_routes: vec![],
            ..config
        };
        assert!(config.resolve_provider("claude-sonnet-4").is_none());
    }

    #[test]
    fn test_resolve_provider_multi_provider_pool() {
        let config = ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![
                ProviderConfig {
                    name: "primary".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://primary.com".into(),
                    api_key: "sk-1".into(),
                    account: None,
                    accounts: Vec::new(),
                    api: None,
                    wire: None,
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                    plugin: None,
                    proxy: None,
                },
                ProviderConfig {
                    name: "backup".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://backup.com".into(),
                    api_key: "sk-2".into(),
                    account: None,
                    accounts: Vec::new(),
                    api: None,
                    wire: None,
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                    plugin: None,
                    proxy: None,
                },
            ],
            model_routes: vec![ModelRouteConfig {
                pattern: "gpt-*".into(),
                provider: None,
                providers: vec!["primary".into(), "backup".into()],
                upstream_model: None,
                strategy: RouteStrategy::Priority,
            }],
            routes: vec![],
            model_metadata: HashMap::new(),
            pricing: Default::default(),
            logging: LoggingConfig::default(),
        };

        let r = config.resolve_provider("gpt-4.1").unwrap();
        assert_eq!(r.provider_names, vec!["primary", "backup"]);
        assert_eq!(r.strategy, RouteStrategy::Priority);
    }

    #[test]
    fn test_validate_config_errors() {
        let config = ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![],
            model_routes: vec![ModelRouteConfig {
                pattern: "gpt-*".into(),
                provider: Some("nonexistent".into()),
                providers: vec![],
                upstream_model: None,
                strategy: RouteStrategy::Priority,
            }],
            routes: vec![RouteConfig {
                client_format: Format::Claude,
                provider: "also-nonexistent".into(),
            }],
            model_metadata: HashMap::new(),
            pricing: Default::default(),
            logging: LoggingConfig::default(),
        };

        let errors = config.validate();
        // Legacy routes are not validated any more: only the model_routes error remains.
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("nonexistent"));
    }

    #[test]
    fn test_available_models() {
        let config = ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
                pass_through_headers: PassThroughHeadersConfig::default(),
                proxy: None,
            },
            providers: vec![ProviderConfig {
                name: "openai".into(),
                format: Format::OpenAIChat,
                base_url: "https://api.openai.com".into(),
                api_key: "sk-test".into(),
                account: None,
                accounts: Vec::new(),
                api: None,
                wire: None,
                model_map: [("gpt-4.1".into(), "gpt-4.1".into())].into(),
                endpoints: Default::default(),
                auth: Default::default(),
                plugin: None,
                proxy: None,
            }],
            model_routes: vec![
                ModelRouteConfig {
                    pattern: "claude-sonnet-4".into(),
                    provider: Some("openai".into()),
                    providers: vec![],
                    upstream_model: None,
                    strategy: RouteStrategy::Priority,
                },
                ModelRouteConfig {
                    pattern: "claude-*".into(),
                    provider: Some("openai".into()),
                    providers: vec![],
                    upstream_model: None,
                    strategy: RouteStrategy::Priority,
                },
                ModelRouteConfig {
                    pattern: "*".into(),
                    provider: Some("openai".into()),
                    providers: vec![],
                    upstream_model: None,
                    strategy: RouteStrategy::Priority,
                },
            ],
            routes: vec![],
            model_metadata: HashMap::new(),
            pricing: Default::default(),
            logging: LoggingConfig::default(),
        };

        let models = config.available_models();
        assert!(models.contains(&"claude-sonnet-4".to_string()));
        assert!(models.contains(&"gpt-4.1".to_string()));
        // wildcards should not appear
        assert!(!models.contains(&"claude-*".to_string()));
        assert!(!models.contains(&"*".to_string()));
    }

    #[test]
    fn test_parse_pass_through_headers_config() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 8080

[server.pass_through_headers]
mode = "deny"
allow = ["x-allowed-*", "openai-organization"]
deny = ["x-internal-*"]
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        assert_eq!(
            config.server.pass_through_headers.mode,
            PassThroughMode::Deny
        );
        assert_eq!(
            config.server.pass_through_headers.allow,
            vec!["x-allowed-*".to_string(), "openai-organization".to_string()]
        );
        assert_eq!(
            config.server.pass_through_headers.deny,
            vec!["x-internal-*".to_string()]
        );
    }

    #[test]
    fn test_parse_logging_config_backward_compatible() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 8080

[logging]
level = "info"
dir = "./logs"

[[logging.files]]
name = "error"
level = "error"
"#;
        let config = ServerConfig::from_toml(toml_str).unwrap();
        assert!(config.logging.console.enabled);
        assert_eq!(config.logging.console.output, "stdout");
        assert_eq!(config.logging.console.format, "pretty");
        assert_eq!(config.logging.files[0].format, "json");
        assert_eq!(config.logging.files[0].rotation, "daily");
    }

    #[cfg(unix)]
    const CONFIG_WITH_KEY: &str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://example.com"
api_key = "sk-plaintext"
"#;

    #[cfg(unix)]
    fn write_config(dir: &tempfile::TempDir, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.path().join("config.toml");
        std::fs::write(&path, CONFIG_WITH_KEY).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_config_with_api_key_warns() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, 0o644);

        assert!(is_world_readable(&path));
        let message = world_readable_warning(&path).expect("0644 config with api_key must warn");
        assert!(message.contains("0644"), "message: {message}");
        assert!(message.contains("chmod 600"), "message: {message}");
        assert!(message.contains("OAuth"), "message: {message}");
    }

    #[cfg(unix)]
    #[test]
    fn private_config_does_not_warn() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, 0o600);

        assert!(!is_world_readable(&path));
        assert!(world_readable_warning(&path).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn warn_does_not_change_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, 0o644);

        warn_if_world_readable(&path);
        warn_if_world_readable(&path);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "warning must not rewrite permissions");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG_WITH_KEY);
    }

    // ------------------------------------------------------------------
    // Egress proxy (P10): the sign-in/refresh/revocation clients leave
    // through the configured proxy, which receives the absolute URI.
    // ------------------------------------------------------------------

    #[test]
    fn build_proxied_client_accepts_valid_and_rejects_invalid_proxies() {
        assert!(build_proxied_client(Some("http://127.0.0.1:1")).is_ok());
        assert!(build_proxied_client(Some("https://proxy.example.test:8080")).is_ok());
        assert!(build_proxied_client(Some("not a url")).is_err());
        assert!(build_proxied_client(None).is_ok());
    }

    #[tokio::test]
    async fn proxied_client_sends_absolute_uri_to_the_proxy() {
        // A fake HTTP proxy: it records the request target. HTTP proxies
        // receive the absolute URI (`http://host/path`), which proves the
        // client really routed through the proxy.
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
        let app =
            axum::Router::new().fallback(move |req: axum::http::Request<axum::body::Body>| {
                let seen_tx = seen_tx.clone();
                async move {
                    let _ = seen_tx.send(req.uri().to_string());
                    axum::http::StatusCode::OK
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = build_proxied_client(Some(&format!("http://{addr}"))).unwrap();
        let response = client
            .get("http://example.test/v1/accounts")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let seen = seen_rx.recv().await.unwrap();
        assert_eq!(
            seen, "http://example.test/v1/accounts",
            "the proxy must see the absolute URI, not the origin-form path"
        );
    }
}
