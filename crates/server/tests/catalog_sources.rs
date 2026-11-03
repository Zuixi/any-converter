//! `catalog_from_config` normalization: explicit model_map entries and
//! `[[model_routes]]` patterns both contribute; wildcards never do.

use any_converter_server::agents::catalog_from_config;
use any_converter_server::config::ServerConfig;

fn parse(toml: &str) -> ServerConfig {
    ServerConfig::from_toml(toml).unwrap()
}

#[test]
fn model_routes_patterns_enter_the_catalog() {
    // The README's recommended setup routes via model_routes with concrete
    // patterns and no model_map at all — the catalog must not be empty.
    let config = parse(
        r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "deepseek"
format = "openai_chat"
base_url = "https://api.deepseek.com"
api_key = "sk-x"

[[model_routes]]
pattern = "deepseek-chat"
provider = "deepseek"

[[model_routes]]
pattern = "deepseek-reasoner"
providers = ["deepseek"]
"#,
    );
    let catalog = catalog_from_config(&config);
    let ids: Vec<&str> = catalog
        .models
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert!(ids.contains(&"deepseek/deepseek-chat"), "ids={ids:?}");
    assert!(ids.contains(&"deepseek/deepseek-reasoner"), "ids={ids:?}");
    assert_eq!(ids.len(), 2, "ids={ids:?}");
}

#[test]
fn wildcard_patterns_and_model_map_wildcards_are_excluded() {
    let config = parse(
        r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-x"

[providers.model_map]
"gpt-4o*" = "gpt-4o"
"*" = "gpt-4.1-mini"
"gpt-4.1" = "gpt-4.1"

[[model_routes]]
pattern = "gpt-*"
provider = "openai"
"#,
    );
    let catalog = catalog_from_config(&config);
    let ids: Vec<&str> = catalog
        .models
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    // Only the concrete model_map entry survives; `gpt-4o*` and `*` from
    // model_map and `gpt-*` from model_routes are patterns, not models.
    assert_eq!(ids, vec!["openai/gpt-4.1"], "ids={ids:?}");
}

#[test]
fn model_map_and_model_routes_merge_without_duplicates() {
    let config = parse(
        r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "deepseek"
format = "openai_chat"
base_url = "https://api.deepseek.com"
api_key = "sk-x"

[providers.model_map]
"deepseek-chat" = "deepseek-chat"

[[model_routes]]
pattern = "deepseek-chat"
provider = "deepseek"

[[model_routes]]
pattern = "deepseek-r1"
provider = "deepseek"
"#,
    );
    let catalog = catalog_from_config(&config);
    let ids: Vec<&str> = catalog
        .models
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, vec!["deepseek/deepseek-chat", "deepseek/deepseek-r1"], "ids={ids:?}");
}

#[test]
fn default_port_is_1228() {
    // The gateway's public default port; CLI, WebUI proxy, and Desktop all
    // hard-code the same number, so this pins the contract.
    let config = parse(
        r#"
[server]
host = "127.0.0.1"
"#,
    );
    assert_eq!(config.server.port, 1228);
}
