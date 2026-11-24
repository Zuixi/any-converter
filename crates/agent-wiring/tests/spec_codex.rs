#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use agent_wiring::edit::{KV, json, toml};
use agent_wiring::spec::codex::{CodexSpec, OUR_KEYS};
use agent_wiring::spec::{AgentSpec, DetectResult, WiredState, WiringCtx};
use agent_wiring::wiring::Wiring;
use serde_json::json;
use tempfile::tempdir;

fn wiring_ctx<'a>(catalog: &'a agent_wiring::ModelCatalog) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: "gpt-5-codex",
        effort: None,
        roles: &common::NO_ROLES,
    }
}

#[test]
fn detect_respects_codex_home_override() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    assert_eq!(CodexSpec.detect(&ctx), DetectResult::Absent);

    let codex_home = dir.path().join("custom-codex");
    fs::create_dir_all(&codex_home).unwrap();
    common::write(&codex_home.join("config.toml"), "model = \"old\"\n");
    let ctx = common::home_with(
        dir.path(),
        &[("CODEX_HOME", codex_home.to_string_lossy().into_owned())],
    );
    assert_eq!(
        CodexSpec.detect(&ctx),
        DetectResult::Detected {
            config_dir: codex_home
        }
    );
}

/// The value `config.toml` must give `model_catalog_json`: a basename on
/// Windows (a `C:\` path is no file to a Codex reading the config from WSL),
/// absolute elsewhere.
fn expected_catalog_ref(catalog_file: &std::path::Path) -> String {
    if cfg!(windows) {
        catalog_file
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    } else {
        catalog_file.to_string_lossy().into_owned()
    }
}

#[test]
fn connect_writes_provider_table_and_preserves_oauth_tokens() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    let auth = dir.path().join(".codex/auth.json");
    let catalog_file = dir.path().join(".codex").join("any-converter-models.json");
    common::write(&config, "# keep\nmodel_provider = \"openai\"\n");
    let original_auth =
        "{\"tokens\": {\"access_token\": \"oauth-token\", \"refresh_token\": \"refresh\"}}";
    common::write(&auth, original_auth);
    let catalog = common::catalog();

    let written = CodexSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    assert!(!written.contains(&auth), "connect must not write auth.json");

    let config_text = common::read(&config);
    assert_eq!(
        toml::get(&config_text, "model_provider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        toml::get(&config_text, "model").as_deref(),
        Some("gpt-5-codex")
    );
    assert_eq!(
        toml::get(&config_text, "model_providers.any-converter.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        toml::get(&config_text, "model_providers.any-converter.wire_api").as_deref(),
        Some("responses")
    );
    // The key lives in the provider table, never in auth.json.
    assert_eq!(
        toml::get(
            &config_text,
            "model_providers.any-converter.experimental_bearer_token"
        )
        .as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        toml::get(
            &config_text,
            "model_providers.any-converter.http_headers.x-openai-actor-authorization"
        )
        .as_deref(),
        Some("any-converter")
    );
    // No ChatGPT sign-in means the field is omitted, as magpie writes it.
    assert_eq!(
        toml::get(
            &config_text,
            "model_providers.any-converter.requires_openai_auth"
        ),
        None
    );
    assert_eq!(
        toml::get(&config_text, "model_catalog_json").as_deref(),
        Some(expected_catalog_ref(&catalog_file).as_str())
    );

    // auth.json is byte-for-byte untouched.
    assert_eq!(common::read(&auth), original_auth);

    let catalog_text = common::read(&catalog_file);
    let parsed: serde_json::Value = serde_json::from_str(&catalog_text).unwrap();
    assert!(parsed["models"].as_array().unwrap().len() >= 3);
    assert_eq!(CodexSpec.our_keys(), OUR_KEYS);
    assert_eq!(
        CodexSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
}

#[test]
fn catalog_entries_carry_every_field_codex_requires() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    let catalog_file = dir.path().join(".codex").join("any-converter-models.json");
    common::write(&config, "");
    let catalog = common::catalog();
    CodexSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&common::read(&catalog_file)).unwrap();
    let models = parsed["models"].as_array().unwrap();

    // A reasoning model: levels, summaries, and the medium default.
    let reasoning = models
        .iter()
        .find(|model| model["slug"] == "claude-opus-4-5")
        .unwrap();
    for field in [
        "slug",
        "display_name",
        "description",
        "base_instructions",
        "shell_type",
        "visibility",
        "supported_in_api",
        "priority",
        "support_verbosity",
        "apply_patch_tool_type",
        "truncation_policy",
        "experimental_supported_tools",
        "input_modalities",
        "service_tiers",
        "supports_search_tool",
        "supports_parallel_tool_calls",
        "supports_reasoning_summaries",
        "supported_reasoning_levels",
        "default_reasoning_level",
        // Written as null, never omitted: the field has no omitempty.
        "default_verbosity",
    ] {
        assert!(
            reasoning.get(field).is_some(),
            "catalog entry lacks `{field}`: {reasoning}"
        );
    }
    assert_eq!(reasoning["visibility"], "list");
    assert_eq!(reasoning["supported_in_api"], true);
    assert_eq!(reasoning["input_modalities"], serde_json::json!(["text"]));
    assert_eq!(reasoning["shell_type"], "unified_exec");
    assert_eq!(reasoning["apply_patch_tool_type"], "freeform");
    assert_eq!(reasoning["supports_search_tool"], true);
    assert_eq!(reasoning["supports_parallel_tool_calls"], true);
    assert_eq!(reasoning["supports_reasoning_summaries"], true);
    assert_eq!(reasoning["default_reasoning_level"], "medium");
    assert_eq!(reasoning["context_window"], 200_000);
    assert_eq!(reasoning["service_tiers"], serde_json::json!([]));
    assert_eq!(
        reasoning["experimental_supported_tools"],
        serde_json::json!([])
    );
    assert_eq!(reasoning["truncation_policy"]["mode"], "tokens");
    assert!(!reasoning["base_instructions"].as_str().unwrap().is_empty());
    let levels = reasoning["supported_reasoning_levels"].as_array().unwrap();
    assert!(levels.len() >= 2);
    for level in levels {
        assert!(level["effort"].as_str().is_some());
        // Codex 0.166 fails the whole catalog on a level without a description.
        assert!(level["description"].as_str().is_some());
    }

    // A non-reasoning model: no levels, no summaries, bare default.
    let plain = models
        .iter()
        .find(|model| model["slug"] == "claude-haiku-4-5")
        .unwrap();
    assert_eq!(plain["supports_reasoning_summaries"], false);
    assert_eq!(plain["default_reasoning_level"], serde_json::Value::Null);
    assert_eq!(plain["supported_reasoning_levels"], serde_json::json!([]));
}

#[test]
fn drift_is_detected_from_provider_base_url() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    common::write(&config, "");
    let catalog = common::catalog();
    CodexSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let text = common::read(&config);
    let drifted = toml::set(
        &text,
        &[KV::new(
            "model_providers.any-converter.base_url",
            json!("https://api.openai.com/v1"),
        )],
    )
    .unwrap();
    common::write(&config, &drifted);

    assert_eq!(
        CodexSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Drifted
    );
}

#[test]
fn direct_disconnect_restores_old_values_and_removes_catalog() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    let auth = dir.path().join(".codex/auth.json");
    let catalog_file = dir.path().join(".codex").join("any-converter-models.json");
    common::write(
        &config,
        "model_provider = \"openai\"\nmodel = \"gpt-old\"\n",
    );
    common::write(
        &auth,
        "{\"OPENAI_API_KEY\": \"sk-old\", \"tokens\": {\"access_token\": \"t\"}}",
    );
    let catalog = common::catalog();
    CodexSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    assert!(catalog_file.exists());

    CodexSpec.disconnect(&ctx).unwrap();

    let config_text = common::read(&config);
    assert_eq!(
        toml::get(&config_text, "model_provider").as_deref(),
        Some("openai")
    );
    assert_eq!(toml::get(&config_text, "model").as_deref(), Some("gpt-old"));
    assert_eq!(toml::get(&config_text, "model_catalog_json"), None);
    // The provider table stays so existing threads can be reopened; only our
    // secret and header go.
    assert_eq!(
        toml::get(&config_text, "model_providers.any-converter.name").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        toml::get(&config_text, "model_providers.any-converter.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        toml::get(
            &config_text,
            "model_providers.any-converter.experimental_bearer_token"
        ),
        None
    );
    assert_eq!(
        toml::get(
            &config_text,
            "model_providers.any-converter.http_headers.x-openai-actor-authorization"
        ),
        None
    );
    // The user's own key was never touched.
    let auth_text = common::read(&auth);
    assert_eq!(
        json::get(&auth_text, "OPENAI_API_KEY").as_deref(),
        Some("sk-old")
    );
    assert_eq!(
        json::get(&auth_text, "tokens.access_token").as_deref(),
        Some("t")
    );
    assert!(!catalog_file.exists());
    // The kept table must not read as connected or drifted after a disconnect.
    assert_eq!(
        CodexSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn roundtrip_keeps_provider_table_and_restores_auth() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    let auth = dir.path().join(".codex/auth.json");
    let catalog_file = dir.path().join(".codex").join("any-converter-models.json");
    let original_config = concat!(
        "# keep this comment\r\n",
        "model_provider = \"openai\"\r\n",
        "model = \"gpt-old\"\r\n",
        "\r\n",
        "[model_providers.openai]\r\n",
        "name = \"OpenAI\"\r\n"
    );
    let original_auth =
        "{\n  // oauth comment\n  \"tokens\": {\"access_token\": \"oauth-token\"}\n}\n";
    common::write(&config, original_config);
    common::write(&auth, original_auth);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&CodexSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert!(common::read(&config).contains("# keep this comment"));
    assert!(catalog_file.exists());

    let report = wiring.disconnect(&CodexSpec, &ctx).unwrap();
    // The provider table deliberately outlives the wiring, so the file no
    // longer matches its pre-connect bytes and the backup is kept.
    assert_eq!(report.kept_backup, vec![config.clone()]);
    let config_text = common::read(&config);
    assert!(config_text.contains("# keep this comment"));
    assert_eq!(
        toml::get(&config_text, "model_provider").as_deref(),
        Some("openai")
    );
    assert_eq!(toml::get(&config_text, "model").as_deref(), Some("gpt-old"));
    assert_eq!(
        toml::get(&config_text, "model_providers.openai.name").as_deref(),
        Some("OpenAI")
    );
    assert_eq!(
        toml::get(&config_text, "model_providers.any-converter.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        toml::get(
            &config_text,
            "model_providers.any-converter.experimental_bearer_token"
        ),
        None
    );
    // auth.json was never written, so it is byte-identical.
    assert_eq!(fs::read(&auth).unwrap(), original_auth.as_bytes());
    assert!(!catalog_file.exists());
}

#[test]
fn user_edit_survives_disconnect() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    common::write(
        &config,
        "model_provider = \"openai\"\nmodel = \"gpt-old\"\n",
    );
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());
    wiring
        .connect(&CodexSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&config);
    let edited = toml::set(&text, &[KV::new("approval_policy", json!("never"))]).unwrap();
    common::write(&config, &edited);

    let report = wiring.disconnect(&CodexSpec, &ctx).unwrap();
    let text = common::read(&config);
    assert_eq!(
        toml::get(&text, "approval_policy").as_deref(),
        Some("never")
    );
    assert_eq!(report.kept_backup, vec![config]);
}
