use std::collections::HashMap;
use std::path::Path;
#[cfg(unix)]
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use any_converter_core::convert::Format;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Deserialize)]
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
}

/// Transport wire used to reach a provider's upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wire {
    /// Regular provider APIs (Gemini/Claude/OpenAI chat/responses paths).
    GeminiApi,
    /// Gemini Code Assist internal endpoints (`cloudcode-pa.googleapis.com`)
    /// with the `{"model", "request"}` envelope and OAuth bearer auth.
    CodeAssist,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub format: Format,
    pub base_url: String,
    pub api_key: String,
    /// Optional account id in the secret store (e.g. `gemini:me@example.com`).
    /// When set, requests are signed with that account's OAuth access token
    /// and `api_key` is ignored.
    #[serde(default)]
    pub account: Option<String>,
    /// Explicit wire override; otherwise inferred via [`Self::effective_wire`].
    #[serde(default)]
    pub wire: Option<Wire>,
    #[serde(default)]
    pub model_map: HashMap<String, String>,
    #[serde(default)]
    pub endpoints: ProviderEndpointConfig,
    #[serde(default)]
    pub auth: ProviderAuthConfig,
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    Bearer,
    ApiKeyHeader,
    XApiKey,
    GoogleApiKey,
    Anthropic,
    None,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelRouteConfig {
    pub pattern: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub strategy: RouteStrategy,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    #[default]
    Priority,
    RoundRobin,
}

/// Legacy format-based route. Not used for routing any more; kept so old configs still parse.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    pub client_format: Format,
    pub provider: String,
}

/// Result of model-based provider resolution.
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    pub provider_names: Vec<String>,
    pub upstream_model: String,
    pub strategy: RouteStrategy,
}

impl ModelRouteConfig {
    /// Collect all provider names (merging `provider` and `providers` fields).
    pub fn provider_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(ref p) = self.provider {
            names.push(p.clone());
        }
        names.extend(self.providers.iter().cloned());
        names
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelMetadata {
    #[serde(default)]
    pub context_window: Option<i64>,
    #[serde(default)]
    pub max_context_window: Option<i64>,
    #[serde(default)]
    pub supports_parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

// ── Logging ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default = "default_max_disk_mb")]
    pub max_disk_mb: u64,
    #[serde(default = "default_true")]
    pub conversion_log: bool,
    /// Log the headers sent to the upstream provider (sensitive values redacted).
    #[serde(default = "default_false")]
    pub upstream_headers_log: bool,
    #[serde(default)]
    pub console: ConsoleConfig,
    #[serde(default)]
    pub files: Vec<LogFileConfig>,
    #[serde(default)]
    pub request_log: RequestLogConfig,
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
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequestLogConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
    #[serde(default = "default_max_capture_bytes")]
    pub max_capture_bytes: usize,
    #[serde(default = "default_true")]
    pub trace_enabled: bool,
    #[serde(default = "default_trace_preview_bytes")]
    pub trace_max_preview_bytes: usize,
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

#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_console_output")]
    pub output: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default = "default_console_format")]
    pub format: String,
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

fn default_console_output() -> String {
    "stdout".to_string()
}

fn default_console_format() -> String {
    "pretty".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogFileConfig {
    pub name: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "default_file_format")]
    pub format: String,
    #[serde(default = "default_rotation")]
    pub rotation: String,
}

fn default_file_format() -> String {
    "json".to_string()
}

fn default_rotation() -> String {
    "daily".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_max_disk_mb() -> u64 {
    500
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_max_capture_bytes() -> usize {
    10 * 1024 * 1024
}

fn default_trace_preview_bytes() -> usize {
    2048
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

impl ProviderConfig {
    /// Resolve the wire used to talk to this provider.
    ///
    /// An explicit `wire` always wins. Otherwise an `account` whose provider
    /// prefix (the part before `:`) is `gemini` or `antigravity` selects
    /// [`Wire::CodeAssist`]; everything else stays on [`Wire::GeminiApi`].
    pub fn effective_wire(&self) -> Wire {
        if let Some(wire) = self.wire {
            return wire;
        }
        let prefix = self.account.as_deref().and_then(|a| a.split(':').next());
        match prefix {
            Some("gemini" | "antigravity") => Wire::CodeAssist,
            _ => Wire::GeminiApi,
        }
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

impl ServerConfig {
    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(content)
    }

    /// Resolve a provider for the given model name via `[[model_routes]]` globs.
    ///
    /// Legacy `[[routes]]` are ignored (see the CLI startup warning).
    pub fn resolve_provider(&self, client_model: &str) -> Option<ResolvedRoute> {
        if !self.model_routes.is_empty() {
            for mr in &self.model_routes {
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

    pub fn find_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// Validate config references at startup. Returns a list of warnings/errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let provider_names: Vec<&str> = self.providers.iter().map(|p| p.name.as_str()).collect();

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

/// `$ANY_CONVERTER_SECRETS` overrides the store location; otherwise it is
/// `<config_dir>/secrets.json` where `config_dir` is
/// `$HOME/.config/any-converter` (or `$APPDATA/any-converter` on Windows).
///
/// Shared by the server runtime and the CLI so both agree on the account
/// store location.
pub fn secrets_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("ANY_CONVERTER_SECRETS") {
        return std::path::PathBuf::from(path);
    }
    config_dir().join("secrets.json")
}

fn config_dir() -> std::path::PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        return std::path::PathBuf::from(appdata).join("any-converter");
    }
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

    fn sample_provider(model_map: HashMap<String, String>) -> ProviderConfig {
        ProviderConfig {
            name: "openai".into(),
            format: Format::OpenAIChat,
            base_url: "https://api.openai.com".into(),
            api_key: "sk-test".into(),
            account: None,
            wire: None,
            model_map,
            endpoints: Default::default(),
            auth: Default::default(),
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
        assert_eq!(config.request_log.max_capture_bytes, 10 * 1024 * 1024);
        assert!(config.request_log.trace_enabled);
        assert_eq!(config.request_log.trace_max_preview_bytes, 2048);
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
    fn test_effective_wire_defaults_and_overrides() {
        let mut provider = sample_provider(HashMap::new());
        assert_eq!(provider.effective_wire(), Wire::GeminiApi);

        provider.account = Some("gemini:me@example.com".into());
        assert_eq!(provider.effective_wire(), Wire::CodeAssist);

        provider.account = Some("antigravity:me@example.com".into());
        assert_eq!(provider.effective_wire(), Wire::CodeAssist);

        provider.account = Some("copilot:me".into());
        assert_eq!(provider.effective_wire(), Wire::GeminiApi);

        // Explicit wire always wins over account inference.
        provider.wire = Some(Wire::GeminiApi);
        provider.account = Some("gemini:me@example.com".into());
        assert_eq!(provider.effective_wire(), Wire::GeminiApi);

        provider.wire = Some(Wire::CodeAssist);
        provider.account = None;
        assert_eq!(provider.effective_wire(), Wire::CodeAssist);
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
        assert_eq!(provider.wire, Some(Wire::CodeAssist));
        assert_eq!(provider.effective_wire(), Wire::CodeAssist);
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
            },
            providers: vec![
                ProviderConfig {
                    name: "anthropic".into(),
                    format: Format::Claude,
                    base_url: "https://api.anthropic.com".into(),
                    api_key: "sk-ant".into(),
                    account: None,
                    wire: None,
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                },
                ProviderConfig {
                    name: "openai".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://api.openai.com".into(),
                    api_key: "sk-proj".into(),
                    account: None,
                    wire: None,
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
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
            wire: None,
            model_map: [("*".into(), "gpt-4.1".into())].into(),
            endpoints: Default::default(),
            auth: Default::default(),
        };
        let server = ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
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
            },
            providers: vec![
                ProviderConfig {
                    name: "primary".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://primary.com".into(),
                    api_key: "sk-1".into(),
                    account: None,
                    wire: None,
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                },
                ProviderConfig {
                    name: "backup".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://backup.com".into(),
                    api_key: "sk-2".into(),
                    account: None,
                    wire: None,
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
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
            },
            providers: vec![ProviderConfig {
                name: "openai".into(),
                format: Format::OpenAIChat,
                base_url: "https://api.openai.com".into(),
                api_key: "sk-test".into(),
                account: None,
                wire: None,
                model_map: [("gpt-4.1".into(), "gpt-4.1".into())].into(),
                endpoints: Default::default(),
                auth: Default::default(),
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
}
