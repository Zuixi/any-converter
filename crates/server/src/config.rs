use std::collections::HashMap;

use any_converter_core::convert::Format;
use serde::Deserialize;

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

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub format: Format,
    pub base_url: String,
    pub api_key: String,
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

/// Legacy format-based route (backward compatibility).
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
}

impl Default for RequestLogConfig {
    fn default() -> Self {
        Self {
            enabled: default_false(),
            max_capture_bytes: default_max_capture_bytes(),
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

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

impl ProviderConfig {
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

    /// Resolve a provider for the given client format and model name.
    ///
    /// Priority: model_routes (first glob match) → legacy routes (format match) → None.
    pub fn resolve_provider(
        &self,
        client_format: Format,
        client_model: &str,
    ) -> Option<ResolvedRoute> {
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
        let route = self.find_route(client_format)?;
        let provider = self.find_provider(&route.provider)?;
        let upstream = provider.resolve_model(client_model);
        Some(ResolvedRoute {
            provider_names: vec![route.provider.clone()],
            upstream_model: upstream,
            strategy: RouteStrategy::Priority,
        })
    }

    pub fn find_route(&self, client_format: Format) -> Option<&RouteConfig> {
        self.routes
            .iter()
            .find(|r| r.client_format == client_format)
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
        for (i, route) in self.routes.iter().enumerate() {
            if !provider_names.contains(&route.provider.as_str()) {
                errors.push(format!(
                    "routes[{}] (format={}): provider \"{}\" not found",
                    i, route.client_format, route.provider
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

/// Simple glob matching: supports `*` as a wildcard segment.
///
/// - `"*"` matches everything
/// - `"claude-*"` matches any string starting with `"claude-"`
/// - `"*-turbo"` matches any string ending with `"-turbo"`
/// - `"gpt-4.1"` matches exactly `"gpt-4.1"`
fn glob_match(pattern: &str, value: &str) -> bool {
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
    fn test_find_route() {
        let config = ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
            },
            providers: vec![],
            model_routes: vec![],
            routes: vec![RouteConfig {
                client_format: Format::Claude,
                provider: "openai".into(),
            }],
            model_metadata: HashMap::new(),
            logging: LoggingConfig::default(),
        };
        assert!(config.find_route(Format::Claude).is_some());
        assert!(config.find_route(Format::Gemini).is_none());
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
            },
            providers: vec![
                ProviderConfig {
                    name: "anthropic".into(),
                    format: Format::Claude,
                    base_url: "https://api.anthropic.com".into(),
                    api_key: "sk-ant".into(),
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                },
                ProviderConfig {
                    name: "openai".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://api.openai.com".into(),
                    api_key: "sk-proj".into(),
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

        let r = config
            .resolve_provider(Format::Claude, "claude-sonnet-4")
            .unwrap();
        assert_eq!(r.provider_names, vec!["anthropic"]);
        assert_eq!(r.upstream_model, "claude-sonnet-4");

        let r = config
            .resolve_provider(Format::OpenAIChat, "gpt-4.1")
            .unwrap();
        assert_eq!(r.provider_names, vec!["openai"]);
        assert_eq!(r.upstream_model, "gpt-4.1");

        // Wildcard fallback with upstream_model override
        let r = config
            .resolve_provider(Format::Claude, "unknown-model")
            .unwrap();
        assert_eq!(r.provider_names, vec!["openai"]);
        assert_eq!(r.upstream_model, "gpt-4.1-mini");
    }

    #[test]
    fn test_resolve_provider_fallback_to_legacy_routes() {
        let config = ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
            },
            providers: vec![ProviderConfig {
                name: "openai".into(),
                format: Format::OpenAIChat,
                base_url: "https://api.openai.com".into(),
                api_key: "sk-proj".into(),
                model_map: [("*".into(), "gpt-4.1".into())].into(),
                endpoints: Default::default(),
                auth: Default::default(),
            }],
            model_routes: vec![],
            routes: vec![RouteConfig {
                client_format: Format::Claude,
                provider: "openai".into(),
            }],
            model_metadata: HashMap::new(),
            logging: LoggingConfig::default(),
        };

        let r = config
            .resolve_provider(Format::Claude, "claude-sonnet-4")
            .unwrap();
        assert_eq!(r.provider_names, vec!["openai"]);
        assert_eq!(r.upstream_model, "gpt-4.1");
    }

    #[test]
    fn test_resolve_provider_multi_provider_pool() {
        let config = ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
            },
            providers: vec![
                ProviderConfig {
                    name: "primary".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://primary.com".into(),
                    api_key: "sk-1".into(),
                    model_map: HashMap::new(),
                    endpoints: Default::default(),
                    auth: Default::default(),
                },
                ProviderConfig {
                    name: "backup".into(),
                    format: Format::OpenAIChat,
                    base_url: "https://backup.com".into(),
                    api_key: "sk-2".into(),
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

        let r = config
            .resolve_provider(Format::OpenAIChat, "gpt-4.1")
            .unwrap();
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
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("nonexistent"));
        assert!(errors[1].contains("also-nonexistent"));
    }

    #[test]
    fn test_available_models() {
        let config = ServerConfig {
            server: ServerSettings {
                host: "127.0.0.1".into(),
                port: 8080,
                api_key: None,
            },
            providers: vec![ProviderConfig {
                name: "openai".into(),
                format: Format::OpenAIChat,
                base_url: "https://api.openai.com".into(),
                api_key: "sk-test".into(),
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
}
