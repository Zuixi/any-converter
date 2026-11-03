//! `resolve_provider` must accept the catalog ids agent wiring writes into
//! agent configs (`<provider>/<model>`), not just bare model names.

use any_converter_server::config::ServerConfig;

const CONFIG: &str = r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "deepseek"
format = "openai_chat"
base_url = "https://api.deepseek.com"
api_key = "sk-x"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-y"

[providers.model_map]
"deepseek-chat" = "deepseek-chat"

[[model_routes]]
pattern = "deepseek-chat"
provider = "deepseek"

[[model_routes]]
pattern = "gpt-*"
provider = "openai"

[[model_routes]]
pattern = "*"
provider = "deepseek"
upstream_model = "deepseek-chat"
"#;

#[test]
fn catalog_id_resolves_through_concrete_pattern() {
    let config = ServerConfig::from_toml(CONFIG).unwrap();
    // The exact form agent wiring writes into Claude Code's ANTHROPIC_MODEL.
    let resolved = config.resolve_provider("deepseek/deepseek-chat").unwrap();
    assert_eq!(resolved.provider_names, vec!["deepseek"]);
    assert_eq!(resolved.upstream_model, "deepseek-chat");
}

#[test]
fn catalog_id_resolves_through_glob_pattern() {
    let config = ServerConfig::from_toml(CONFIG).unwrap();
    let resolved = config.resolve_provider("openai/gpt-4o").unwrap();
    assert_eq!(resolved.provider_names, vec!["openai"]);
    assert_eq!(resolved.upstream_model, "gpt-4o");
}

#[test]
fn bare_model_names_still_resolve_unchanged() {
    let config = ServerConfig::from_toml(CONFIG).unwrap();
    let resolved = config.resolve_provider("deepseek-chat").unwrap();
    assert_eq!(resolved.provider_names, vec!["deepseek"]);
    assert_eq!(resolved.upstream_model, "deepseek-chat");
}

#[test]
fn slash_model_without_matching_provider_prefix_keeps_full_name() {
    let config = ServerConfig::from_toml(CONFIG).unwrap();
    // A vendor's own dated id containing a slash: no provider is named
    // "claude", so the full string must be matched as-is (catch-all here).
    let resolved = config.resolve_provider("claude/claude-opus-4-5").unwrap();
    assert_eq!(resolved.provider_names, vec!["deepseek"]);
    // Catch-all pins the upstream model explicitly.
    assert_eq!(resolved.upstream_model, "deepseek-chat");
}

#[test]
fn catalog_id_through_catch_all_keeps_pinned_upstream() {
    let config = ServerConfig::from_toml(CONFIG).unwrap();
    let resolved = config.resolve_provider("deepseek/whatever").unwrap();
    assert_eq!(resolved.provider_names, vec!["deepseek"]);
    assert_eq!(resolved.upstream_model, "deepseek-chat");
}

#[test]
fn unknown_slash_model_without_catch_all_is_not_found() {
    let toml = r#"
[server]
host = "127.0.0.1"

[[providers]]
name = "deepseek"
format = "openai_chat"
base_url = "https://x"
api_key = "sk"

[[model_routes]]
pattern = "deepseek-chat"
provider = "deepseek"
"#;
    let config = ServerConfig::from_toml(toml).unwrap();
    assert!(config.resolve_provider("deepseek/nope").is_none());
    assert!(config.resolve_provider("nope/nope").is_none());
}
