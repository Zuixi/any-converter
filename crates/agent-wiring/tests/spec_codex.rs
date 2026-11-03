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

#[test]
fn connect_sets_requires_openai_auth_and_preserves_oauth_tokens() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    let auth = dir.path().join(".codex/auth.json");
    let catalog_file = dir.path().join(".codex/any-converter-models.json");
    common::write(&config, "# keep\nmodel_provider = \"openai\"\n");
    common::write(
        &auth,
        "{\"tokens\": {\"access_token\": \"oauth-token\", \"refresh_token\": \"refresh\"}}",
    );
    let catalog = common::catalog();

    CodexSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

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
    assert_eq!(
        toml::get(
            &config_text,
            "model_providers.any-converter.requires_openai_auth"
        )
        .as_deref(),
        Some("true")
    );
    assert_eq!(
        toml::get(&config_text, "model_catalog_json").as_deref(),
        Some(catalog_file.to_string_lossy().as_ref())
    );

    let auth_text = common::read(&auth);
    assert_eq!(
        json::get(&auth_text, "OPENAI_API_KEY").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        json::get(&auth_text, "tokens.access_token").as_deref(),
        Some("oauth-token")
    );
    assert_eq!(
        json::get(&auth_text, "tokens.refresh_token").as_deref(),
        Some("refresh")
    );

    let catalog_text = common::read(&catalog_file);
    let parsed: serde_json::Value = serde_json::from_str(&catalog_text).unwrap();
    assert!(parsed["models"].as_array().unwrap().len() >= 3);
    assert_eq!(CodexSpec.our_keys(), OUR_KEYS);
    assert_eq!(CodexSpec.read_state(&ctx, common::GATEWAY), WiredState::Connected);
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

    assert_eq!(CodexSpec.read_state(&ctx, common::GATEWAY), WiredState::Drifted);
}

#[test]
fn direct_disconnect_restores_old_values_and_removes_catalog() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    let auth = dir.path().join(".codex/auth.json");
    let catalog_file = dir.path().join(".codex/any-converter-models.json");
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
    assert_eq!(
        toml::get(&config_text, "model_providers.any-converter"),
        None
    );
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
}

#[test]
fn golden_roundtrip_is_byte_identical() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let config = dir.path().join(".codex/config.toml");
    let auth = dir.path().join(".codex/auth.json");
    let catalog_file = dir.path().join(".codex/any-converter-models.json");
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
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&config).unwrap(), original_config.as_bytes());
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
