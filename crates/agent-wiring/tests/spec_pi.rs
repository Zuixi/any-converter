#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use agent_wiring::edit::{KV, json};
use agent_wiring::spec::pi::{OUR_KEYS, PiSpec};
use agent_wiring::spec::{AgentSpec, DetectResult, WiredState, WiringCtx};
use agent_wiring::wiring::Wiring;
use serde_json::json;
use tempfile::tempdir;

fn wiring_ctx<'a>(catalog: &'a agent_wiring::ModelCatalog) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: "claude-sonnet-4-5",
    }
}

#[test]
fn detect_expands_pi_coding_agent_dir() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    assert_eq!(PiSpec.detect(&ctx), DetectResult::Absent);

    let custom = dir.path().join("custom-pi");
    fs::create_dir_all(&custom).unwrap();
    let ctx = common::home_with(
        dir.path(),
        &[("PI_CODING_AGENT_DIR", "~/custom-pi".to_owned())],
    );
    assert_eq!(
        PiSpec.detect(&ctx),
        DetectResult::Detected { config_dir: custom }
    );
}

#[test]
fn detect_uses_default_pi_agent_dir() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let agent = dir.path().join(".pi/agent");
    fs::create_dir_all(&agent).unwrap();
    assert_eq!(
        PiSpec.detect(&ctx),
        DetectResult::Detected { config_dir: agent }
    );
}

#[test]
fn connect_writes_provider_and_settings() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".pi/agent/settings.json");
    let models = dir.path().join(".pi/agent/models.json");
    common::write(&settings, "{\"enabledModels\": [\"openai/gpt\"]}\n");
    common::write(&models, "{\"providers\": {}}\n");
    let catalog = common::catalog();

    PiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let models_text = common::read(&models);
    assert_eq!(
        json::get(&models_text, "providers.any-converter.name").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&models_text, "providers.any-converter.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&models_text, "providers.any-converter.apiKey").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        json::get(
            &models_text,
            "providers.any-converter.models.claude-sonnet-4-5.name"
        )
        .as_deref(),
        Some("Claude Sonnet 4.5")
    );
    assert_eq!(
        json::get(
            &models_text,
            "providers.any-converter.models.claude-sonnet-4-5.reasoning"
        )
        .as_deref(),
        Some("true")
    );

    let settings_text = common::read(&settings);
    assert_eq!(
        json::get(&settings_text, "defaultProvider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&settings_text, "defaultModel").as_deref(),
        Some("claude-sonnet-4-5")
    );
    let enabled = json::get(&settings_text, "enabledModels").unwrap();
    assert!(enabled.contains("openai/gpt"));
    assert!(enabled.contains("any-converter/claude-sonnet-4-5"));
    assert_eq!(PiSpec.our_keys(), OUR_KEYS);
    assert_eq!(
        PiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
}

#[test]
fn enabled_models_absent_stays_absent() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".pi/agent/settings.json");
    common::write(&settings, "{\"theme\": \"dark\"}\n");
    let catalog = common::catalog();
    PiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    assert_eq!(json::get(&common::read(&settings), "enabledModels"), None);
}

#[test]
fn enabled_models_empty_after_strip_deletes_key() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".pi/agent/settings.json");
    common::write(
        &settings,
        "{\"enabledModels\": [\"any-converter/old-model\"]}",
    );
    let catalog = common::catalog();
    PiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    PiSpec.disconnect(&ctx).unwrap();

    assert_eq!(json::get(&common::read(&settings), "enabledModels"), None);
}

#[test]
fn enabled_models_preserves_user_entries_on_strip() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".pi/agent/settings.json");
    common::write(
        &settings,
        "{\"enabledModels\": [\"openai/gpt\", \"any-converter/old\"]}",
    );
    let catalog = common::catalog();
    PiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    PiSpec.disconnect(&ctx).unwrap();

    let enabled = json::get(&common::read(&settings), "enabledModels").unwrap();
    assert_eq!(enabled, "[\"openai/gpt\"]");
}

#[test]
fn drift_is_detected_from_provider_base_url() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let models = dir.path().join(".pi/agent/models.json");
    common::write(&models, "{}");
    let catalog = common::catalog();
    PiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let text = common::read(&models);
    let drifted = json::set(
        &text,
        &[KV::new(
            "providers.any-converter.baseUrl",
            json!("https://openrouter.ai/api/v1"),
        )],
    )
    .unwrap();
    common::write(&models, &drifted);

    assert_eq!(
        PiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Drifted
    );
}

#[test]
fn golden_roundtrip_is_byte_identical() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".pi/agent/settings.json");
    let models = dir.path().join(".pi/agent/models.json");
    let original_settings = concat!(
        "{\r\n",
        "  // settings comment\r\n",
        "  \"theme\": \"dark\",\r\n",
        "  \"enabledModels\": [\"openai/gpt\"]\r\n",
        "}\r\n"
    );
    let original_models = concat!(
        "{\r\n",
        "  // models comment\r\n",
        "  \"providers\": {\"openai\": {\"name\": \"OpenAI\"}}\r\n",
        "}\r\n"
    );
    common::write(&settings, original_settings);
    common::write(&models, original_models);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&PiSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert!(common::read(&settings).contains("// settings comment"));
    assert!(common::read(&models).contains("// models comment"));

    let report = wiring.disconnect(&PiSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&settings).unwrap(), original_settings.as_bytes());
    assert_eq!(fs::read(&models).unwrap(), original_models.as_bytes());
}

#[test]
fn user_edit_survives_disconnect() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".pi/agent/settings.json");
    common::write(&settings, "{\"theme\": \"dark\"}\n");
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());
    wiring
        .connect(&PiSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&settings);
    let edited = json::set(&text, &[KV::new("newSetting", json!(true))]).unwrap();
    common::write(&settings, &edited);

    let report = wiring.disconnect(&PiSpec, &ctx).unwrap();
    let text = common::read(&settings);
    assert_eq!(json::get(&text, "newSetting").as_deref(), Some("true"));
    assert_eq!(report.kept_backup, vec![settings]);
}
