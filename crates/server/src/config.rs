use std::collections::HashMap;

use any_converter_core::convert::Format;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub server: ServerSettings,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    pub client_format: Format,
    pub provider: String,
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
    pub json: bool,
    #[serde(default = "default_true")]
    pub conversion_log: bool,
    #[serde(default)]
    pub files: Vec<LogFileConfig>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            dir: None,
            max_disk_mb: default_max_disk_mb(),
            json: true,
            conversion_log: true,
            files: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogFileConfig {
    pub name: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
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

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

impl ProviderConfig {
    /// Resolve a client model name to the upstream model using model_map.
    ///
    /// Priority: exact match → wildcard `"*"` → passthrough original name.
    pub fn resolve_model(&self, model: &str) -> String {
        if let Some(mapped) = self.model_map.get(model) {
            return mapped.clone();
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

    pub fn find_route(&self, client_format: Format) -> Option<&RouteConfig> {
        self.routes.iter().find(|r| r.client_format == client_format)
    }

    pub fn find_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_provider(model_map: HashMap<String, String>) -> ProviderConfig {
        ProviderConfig {
            name: "openai".into(),
            format: Format::OpenAIChat,
            base_url: "https://api.openai.com".into(),
            api_key: "sk-test".into(),
            model_map,
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
}
