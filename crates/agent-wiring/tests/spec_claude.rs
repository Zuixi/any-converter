#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use agent_wiring::edit::{KV, json};
use agent_wiring::spec::claude::{ClaudeSpec, OUR_KEYS};
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
    }
}

#[test]
fn detect_requires_dir_or_settings() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    assert_eq!(ClaudeSpec.detect(&ctx), DetectResult::Absent);

    fs::create_dir_all(dir.path().join(".claude")).unwrap();
    assert!(matches!(
        ClaudeSpec.detect(&ctx),
        DetectResult::Detected { .. }
    ));
}

#[test]
fn connect_writes_gateway_keys_and_state() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".claude/settings.json");
    common::write(&settings, "{\n  \"theme\": \"dark\"\n}\n");
    let catalog = common::catalog();

    ClaudeSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let text = common::read(&settings);
    assert_eq!(
        json::get(&text, "env.ANTHROPIC_BASE_URL").as_deref(),
        Some(common::GATEWAY)
    );
    assert_eq!(
        json::get(&text, "env.ANTHROPIC_AUTH_TOKEN").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        json::get(&text, "env.ANTHROPIC_MODEL").as_deref(),
        Some(common::DEFAULT_MODEL)
    );
    assert_eq!(
        json::get(&text, "env.ANTHROPIC_DEFAULT_SONNET_MODEL").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&text, "env.ANTHROPIC_DEFAULT_HAIKU_MODEL").as_deref(),
        Some("claude-haiku-4-5")
    );
    assert_eq!(
        json::get(&text, "env.ANTHROPIC_DEFAULT_OPUS_MODEL").as_deref(),
        Some("claude-opus-4-5")
    );
    assert_eq!(json::get(&text, "theme").as_deref(), Some("dark"));
    assert_eq!(ClaudeSpec.our_keys(), OUR_KEYS);
    assert_eq!(ClaudeSpec.read_state(&ctx, common::GATEWAY), WiredState::Connected);
}

#[test]
fn drift_and_disconnect_states() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".claude/settings.json");
    common::write(&settings, "{}");
    assert_eq!(ClaudeSpec.read_state(&ctx, common::GATEWAY), WiredState::Disconnected);

    let catalog = common::catalog();
    ClaudeSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    assert_eq!(ClaudeSpec.read_state(&ctx, common::GATEWAY), WiredState::Connected);

    let text = common::read(&settings);
    let drifted = json::set(
        &text,
        &[KV::new(
            "env.ANTHROPIC_BASE_URL",
            json!("https://api.anthropic.com"),
        )],
    )
    .unwrap();
    common::write(&settings, &drifted);
    assert_eq!(ClaudeSpec.read_state(&ctx, common::GATEWAY), WiredState::Drifted);

    ClaudeSpec.disconnect(&ctx).unwrap();
    assert_eq!(ClaudeSpec.read_state(&ctx, common::GATEWAY), WiredState::Disconnected);
}

#[test]
fn api_key_helper_is_refused() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".claude/settings.json");
    common::write(
        &settings,
        "{\"apiKeyHelper\": \"security find-generic-password\"}",
    );
    let catalog = common::catalog();

    let result = ClaudeSpec.connect(&ctx, &wiring_ctx(&catalog));

    assert!(matches!(result, Err(agent_wiring::WiringError::Refused(_))));
    let text = common::read(&settings);
    assert_eq!(json::get(&text, "env.ANTHROPIC_BASE_URL"), None);
    assert_eq!(
        json::get(&text, "apiKeyHelper").as_deref(),
        Some("security find-generic-password")
    );
}

#[test]
fn anthropic_api_key_is_stashed_removed_and_restored() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".claude/settings.json");
    common::write(
        &settings,
        "{\"env\": {\"ANTHROPIC_API_KEY\": \"sk-ant-old\", \"KEEP\": \"yes\"}}",
    );
    let catalog = common::catalog();

    ClaudeSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    assert_eq!(
        json::get(&common::read(&settings), "env.ANTHROPIC_API_KEY"),
        None
    );

    ClaudeSpec.disconnect(&ctx).unwrap();
    let text = common::read(&settings);
    assert_eq!(
        json::get(&text, "env.ANTHROPIC_API_KEY").as_deref(),
        Some("sk-ant-old")
    );
    assert_eq!(json::get(&text, "env.KEEP").as_deref(), Some("yes"));
}

#[test]
fn golden_roundtrip_is_byte_identical() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".claude/settings.json");
    let original = concat!(
        "{\r\n",
        "  // keep this comment\r\n",
        "  \"env\": {\r\n",
        "    \"KEEP\": \"yes\"\r\n",
        "  }\r\n",
        "}\r\n"
    );
    common::write(&settings, original);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    let report = wiring
        .connect(&ClaudeSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert_eq!(report.files_written, vec![settings.clone()]);
    assert!(common::read(&settings).contains("// keep this comment"));

    let report = wiring.disconnect(&ClaudeSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&settings).unwrap(), original.as_bytes());
}

#[test]
fn user_edit_survives_disconnect_and_keeps_backup() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".claude/settings.json");
    common::write(&settings, "{\n  \"theme\": \"dark\"\n}\n");
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&ClaudeSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&settings);
    let edited = json::set(
        &text,
        &[KV::new(
            "mcpServers",
            json!({"server": {"command": "node"}}),
        )],
    )
    .unwrap();
    common::write(&settings, &edited);

    let report = wiring.disconnect(&ClaudeSpec, &ctx).unwrap();

    let text = common::read(&settings);
    assert_eq!(
        json::get(&text, "mcpServers.server.command").as_deref(),
        Some("node")
    );
    assert_eq!(report.kept_backup, vec![settings.clone()]);
    assert!(report.warnings.contains(&"kept_backup".to_owned()));
    assert!(PathBufExt::backup(&settings).exists());
}

struct PathBufExt;

impl PathBufExt {
    fn backup(path: &std::path::Path) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{}.before-any-converter", path.display()))
    }
}
