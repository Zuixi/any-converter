#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use agent_wiring::edit::{KV, json};
use agent_wiring::spec::qwen::{OUR_KEYS, QwenSpec};
use agent_wiring::spec::{AgentSpec, DetectResult, WiredState, WiringCtx};
use agent_wiring::wiring::Wiring;
use serde_json::json;
use tempfile::tempdir;

fn wiring_ctx<'a>(catalog: &'a agent_wiring::ModelCatalog) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: common::DEFAULT_MODEL,
        effort: None,
        roles: &common::NO_ROLES,
    }
}

#[test]
fn detect_uses_qwen_home() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    assert_eq!(QwenSpec.detect(&ctx), DetectResult::Absent);

    let custom = dir.path().join("custom-qwen");
    fs::create_dir_all(&custom).unwrap();
    let ctx = common::home_with(dir.path(), &[("QWEN_HOME", "~/custom-qwen".to_owned())]);
    assert_eq!(
        QwenSpec.detect(&ctx),
        DetectResult::Detected { config_dir: custom }
    );
}

#[test]
fn connect_appends_provider_entries_and_points_default() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".qwen/settings.json");
    common::write(
        &settings,
        r#"{"modelProviders": {"openai": [{"baseUrl": "https://x", "envKey": "MY_KEY", "id": "gpt", "name": "GPT"}]}}"#,
    );
    let catalog = common::catalog();
    QwenSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let text = common::read(&settings);
    let entries = json::get(&text, "modelProviders.openai").unwrap();
    assert!(entries.contains("MY_KEY"));
    assert!(entries.contains("ANY_CONVERTER_QWEN_API_KEY"));
    assert!(entries.contains("claude-sonnet-4-5"));
    assert_eq!(
        json::get(&text, "model.name").as_deref(),
        Some(common::DEFAULT_MODEL)
    );
    assert_eq!(
        json::get(&text, "model.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "security.auth.selectedType").as_deref(),
        Some("openai")
    );
    assert_eq!(
        json::get(&text, "env.ANY_CONVERTER_QWEN_API_KEY").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(QwenSpec.our_keys(), OUR_KEYS);
    assert_eq!(
        QwenSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
}

#[test]
fn disconnect_restores_user_values_and_entry_order() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".qwen/settings.json");
    common::write(
        &settings,
        r#"{"model": {"name": "mine"}, "modelProviders": {"openai": [{"baseUrl": "https://x", "envKey": "MY_KEY", "id": "gpt", "name": "GPT"}]}}"#,
    );
    let catalog = common::catalog();
    QwenSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    QwenSpec.disconnect(&ctx).unwrap();

    let text = common::read(&settings);
    assert_eq!(json::get(&text, "model.name").as_deref(), Some("mine"));
    assert_eq!(json::get(&text, "model.baseUrl"), None);
    assert_eq!(json::get(&text, "security.auth.selectedType"), None);
    assert_eq!(json::get(&text, "env.ANY_CONVERTER_QWEN_API_KEY"), None);
    assert_eq!(
        json::get(&text, "modelProviders.openai").as_deref(),
        Some(r#"[{"baseUrl":"https://x","envKey":"MY_KEY","id":"gpt","name":"GPT"}]"#)
    );
}

#[test]
fn drift_is_detected_from_provider_base_url() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".qwen/settings.json");
    common::write(&settings, "{}");
    let catalog = common::catalog();
    QwenSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let entries = json::get(&common::read(&settings), "modelProviders.openai").unwrap();
    let drifted = entries.replace("http://127.0.0.1:8787/v1", "https://openrouter.ai/api/v1");
    let text = json::set(
        &common::read(&settings),
        &[KV::new(
            "modelProviders.openai",
            serde_json::from_str::<serde_json::Value>(&drifted).unwrap(),
        )],
    )
    .unwrap();
    common::write(&settings, &text);

    assert_eq!(
        QwenSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Drifted
    );
    assert_eq!(
        QwenSpec.read_state(&ctx, "http://127.0.0.1:9999"),
        WiredState::Drifted
    );
}

#[test]
fn golden_roundtrip_is_byte_identical() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".qwen/settings.json");
    let original = concat!(
        "{\r\n",
        "  // qwen settings\r\n",
        "  \"modelProviders\": {\"openai\": [{\"id\": \"gpt\", \"envKey\": \"MY_KEY\"}]}\r\n",
        "}\r\n"
    );
    common::write(&settings, original);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&QwenSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert!(common::read(&settings).contains("// qwen settings"));

    let report = wiring.disconnect(&QwenSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&settings).unwrap(), original.as_bytes());
}

#[test]
fn user_edit_survives_disconnect() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".qwen/settings.json");
    common::write(&settings, "{\"theme\": \"dark\"}\n");
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());
    wiring
        .connect(&QwenSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&settings);
    let edited = json::set(&text, &[KV::new("newSetting", json!(true))]).unwrap();
    common::write(&settings, &edited);

    let report = wiring.disconnect(&QwenSpec, &ctx).unwrap();
    assert_eq!(
        json::get(&common::read(&settings), "newSetting").as_deref(),
        Some("true")
    );
    assert_eq!(report.kept_backup, vec![settings]);
}
