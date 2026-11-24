#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use agent_wiring::edit::json;
use agent_wiring::spec::droid::{DroidSpec, OUR_KEYS};
use agent_wiring::spec::{AgentSpec, DetectResult, WiredState, WiringCtx};
use agent_wiring::wiring::Wiring;
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
fn detect_uses_factory_home_override() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    assert_eq!(DroidSpec.detect(&ctx), DetectResult::Absent);

    let home = dir.path().join("factory-home");
    fs::create_dir_all(home.join(".factory")).unwrap();
    let ctx = common::home_with(
        dir.path(),
        &[("FACTORY_HOME_OVERRIDE", home.display().to_string())],
    );
    assert_eq!(
        DroidSpec.detect(&ctx),
        DetectResult::Detected {
            config_dir: home.join(".factory")
        }
    );
}

#[test]
fn connect_appends_custom_models_and_sets_default() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".factory/settings.json");
    common::write(
        &settings,
        r#"{"customModels": [{"model": "mine", "id": "custom:mine", "displayName": "Mine"}]}"#,
    );
    let catalog = common::catalog();
    DroidSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let text = common::read(&settings);
    let entries = json::get(&text, "customModels").unwrap();
    assert!(entries.contains("custom:mine"));
    assert!(entries.contains("custom:any-converter/claude-sonnet-4-5"));
    assert!(entries.contains("generic-chat-completion-api"));
    assert!(entries.contains(common::AGENT_KEY));
    assert_eq!(
        json::get(&text, "sessionDefaultSettings.model").as_deref(),
        Some("custom:any-converter/claude-sonnet-4-5")
    );
    assert_eq!(DroidSpec.our_keys(), OUR_KEYS);
    assert_eq!(
        DroidSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
}

#[test]
fn entries_use_the_provider_their_model_name_reads_as() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".factory/settings.json");
    common::write(&settings, "{}");
    let catalog = common::catalog();
    DroidSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let value: serde_json::Value = serde_json::from_str(&common::read(&settings)).unwrap();
    let entry = |id: &str| {
        value["customModels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["model"] == id)
            .unwrap()
    };
    // Claude names: the Anthropic client adds `/v1` itself, so the base is
    // the gateway root.
    let claude = entry("claude-sonnet-4-5");
    assert_eq!(claude["provider"], "anthropic");
    assert_eq!(claude["baseUrl"], common::GATEWAY);
    // GPT names: the OpenAI SDK joins `/v1` itself.
    let gpt = entry("gpt-5-codex");
    assert_eq!(gpt["provider"], "openai");
    assert_eq!(gpt["baseUrl"], format!("{}/v1", common::GATEWAY));
    // Everything else stays on the generic Chat Completions provider.
    let other = entry("gemini-3-pro");
    assert_eq!(other["provider"], "generic-chat-completion-api");
    assert_eq!(other["baseUrl"], format!("{}/v1", common::GATEWAY));
    for id in ["claude-sonnet-4-5", "gpt-5-codex", "gemini-3-pro"] {
        assert_eq!(entry(id)["noImageSupport"], true);
    }
}

#[test]
fn disconnect_restores_user_entries_and_default() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".factory/settings.json");
    common::write(
        &settings,
        r#"{"customModels": [{"model": "mine", "id": "custom:mine"}], "sessionDefaultSettings": {"model": "custom:mine", "theme": "dark"}}"#,
    );
    let catalog = common::catalog();
    DroidSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    DroidSpec.disconnect(&ctx).unwrap();

    let text = common::read(&settings);
    assert_eq!(
        json::get(&text, "sessionDefaultSettings.model").as_deref(),
        Some("custom:mine")
    );
    assert_eq!(
        json::get(&text, "sessionDefaultSettings.theme").as_deref(),
        Some("dark")
    );
    assert_eq!(
        json::get(&text, "customModels").as_deref(),
        Some(r#"[{"id":"custom:mine","model":"mine"}]"#)
    );
}

#[test]
fn drift_is_detected_from_custom_model_base_url() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".factory/settings.json");
    common::write(&settings, "{}");
    let catalog = common::catalog();
    DroidSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let entries = json::get(&common::read(&settings), "customModels").unwrap();
    let drifted = entries.replace(common::AGENT_KEY, "");
    let value: serde_json::Value = serde_json::from_str(&drifted).unwrap();
    let text = json::set(
        &common::read(&settings),
        &[agent_wiring::edit::KV::new("customModels", value)],
    )
    .unwrap();
    common::write(&settings, &text);

    assert_eq!(
        DroidSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Drifted
    );
}

#[test]
fn golden_roundtrip_is_byte_identical() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".factory/settings.json");
    let original = concat!(
        "{\r\n",
        "  // droid settings\r\n",
        "  \"customModels\": [{\"model\": \"mine\", \"id\": \"custom:mine\"}]\r\n",
        "}\r\n"
    );
    common::write(&settings, original);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&DroidSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert!(common::read(&settings).contains("// droid settings"));

    let report = wiring.disconnect(&DroidSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&settings).unwrap(), original.as_bytes());
}
