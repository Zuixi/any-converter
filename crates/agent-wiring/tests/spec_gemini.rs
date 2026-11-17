#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use agent_wiring::edit::env as env_edit;
use agent_wiring::spec::gemini::{GeminiSpec, OUR_KEYS};
use agent_wiring::spec::{AgentSpec, DetectResult, WiredState, WiringCtx};
use agent_wiring::wiring::Wiring;
use tempfile::tempdir;

fn wiring_ctx<'a>(catalog: &'a agent_wiring::ModelCatalog) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: "gemini-3-pro",
        effort: None,
        roles: &common::NO_ROLES,
    }
}

#[test]
fn detect_ignores_antigravity_directories() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    assert_eq!(GeminiSpec.detect(&ctx), DetectResult::Absent);

    // Antigravity owns ~/.gemini too; its folders say nothing about Gemini CLI.
    fs::create_dir_all(dir.path().join(".gemini/antigravity-cli")).unwrap();
    assert_eq!(GeminiSpec.detect(&ctx), DetectResult::Absent);

    common::write(&dir.path().join(".gemini/settings.json"), "{}");
    assert!(matches!(
        GeminiSpec.detect(&ctx),
        DetectResult::Detected { .. }
    ));
}

#[test]
fn detect_finds_gemini_on_path() {
    let dir = tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let name = if cfg!(windows) {
        "gemini.exe"
    } else {
        "gemini"
    };
    common::write(&bin.join(name), "");

    let ctx = common::home_with(dir.path(), &[("PATH", bin.to_string_lossy().into_owned())]);
    assert!(matches!(
        GeminiSpec.detect(&ctx),
        DetectResult::Detected { .. }
    ));
}

#[test]
fn connect_rewrites_oauth_selected_type_and_removes_legacy_key() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let env_path = dir.path().join(".gemini/.env");
    let settings = dir.path().join(".gemini/settings.json");
    common::write(&env_path, "# keep\nGOOGLE_API_KEY=legacy-key\n");
    common::write(
        &settings,
        "{\n  // auth comment\n  \"security\": {\"auth\": {\"selectedType\": \"oauth-personal\"}}\n}\n",
    );
    let catalog = common::catalog();

    GeminiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let env_text = common::read(&env_path);
    assert_eq!(
        env_edit::get(&env_text, "GOOGLE_GEMINI_BASE_URL").as_deref(),
        Some(common::GATEWAY)
    );
    assert_eq!(
        env_edit::get(&env_text, "GEMINI_API_KEY").as_deref(),
        Some(common::AGENT_KEY)
    );
    // The CLI ignores GEMINI_MODEL; the model lives in settings.json.
    assert_eq!(env_edit::get(&env_text, "GEMINI_MODEL"), None);
    assert_eq!(env_edit::get(&env_text, "GOOGLE_API_KEY"), None);
    assert!(env_text.contains("# keep"));

    let settings_text = common::read(&settings);
    assert_eq!(
        agent_wiring::edit::json::get(&settings_text, "security.auth.selectedType").as_deref(),
        Some("gemini-api-key")
    );
    assert_eq!(
        agent_wiring::edit::json::get(&settings_text, "model.name").as_deref(),
        Some("gemini-3-pro")
    );
    assert!(settings_text.contains("// auth comment"));
    assert_eq!(GeminiSpec.our_keys(), OUR_KEYS);
    assert_eq!(
        GeminiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
}

#[test]
fn disconnect_restores_selected_type_and_legacy_key() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let env_path = dir.path().join(".gemini/.env");
    let settings = dir.path().join(".gemini/settings.json");
    common::write(&env_path, "GOOGLE_API_KEY=legacy-key\n");
    common::write(
        &settings,
        "{\"security\": {\"auth\": {\"selectedType\": \"oauth-personal\"}}}",
    );
    let catalog = common::catalog();
    GeminiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    GeminiSpec.disconnect(&ctx).unwrap();

    let env_text = common::read(&env_path);
    assert_eq!(
        env_edit::get(&env_text, "GOOGLE_API_KEY").as_deref(),
        Some("legacy-key")
    );
    assert_eq!(env_edit::get(&env_text, "GEMINI_API_KEY"), None);
    assert_eq!(env_edit::get(&env_text, "GOOGLE_GEMINI_BASE_URL"), None);
    let settings_text = common::read(&settings);
    assert_eq!(
        agent_wiring::edit::json::get(&settings_text, "security.auth.selectedType").as_deref(),
        Some("oauth-personal")
    );
    assert_eq!(
        agent_wiring::edit::json::get(&settings_text, "model.name"),
        None
    );
}

#[test]
fn connect_creates_settings_when_the_cli_has_none() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".gemini/settings.json");
    fs::create_dir_all(dir.path().join(".gemini")).unwrap();
    assert!(!settings.exists());
    let catalog = common::catalog();

    GeminiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    // Without this file and its auth type the CLI still asks for a sign-in
    // even though `.env` names the gateway.
    let settings_text = common::read(&settings);
    assert_eq!(
        agent_wiring::edit::json::get(&settings_text, "security.auth.selectedType").as_deref(),
        Some("gemini-api-key")
    );
    assert_eq!(
        GeminiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    GeminiSpec.disconnect(&ctx).unwrap();
    let settings_text = common::read(&settings);
    assert_eq!(
        agent_wiring::edit::json::get(&settings_text, "security.auth"),
        None
    );
    assert_eq!(agent_wiring::edit::json::get(&settings_text, "model"), None);
}

#[test]
fn drift_is_detected_from_base_url() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let env_path = dir.path().join(".gemini/.env");
    common::write(&env_path, "");
    let catalog = common::catalog();
    GeminiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let text = common::read(&env_path);
    let drifted = env_edit::set(
        &text,
        &[agent_wiring::edit::KV::new(
            "GOOGLE_GEMINI_BASE_URL",
            "https://generativelanguage.googleapis.com",
        )],
    );
    common::write(&env_path, &drifted);

    assert_eq!(
        GeminiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Drifted
    );
}

#[test]
fn drift_is_detected_from_auth_type() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".gemini/settings.json");
    common::write(&settings, "{}");
    let catalog = common::catalog();
    GeminiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let text = common::read(&settings);
    let drifted = agent_wiring::edit::json::set(
        &text,
        &[agent_wiring::edit::KV::new(
            "security.auth.selectedType",
            serde_json::json!("oauth-personal"),
        )],
    )
    .unwrap();
    common::write(&settings, &drifted);

    assert_eq!(
        GeminiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Drifted
    );
}

#[test]
fn golden_roundtrip_is_byte_identical() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let env_path = dir.path().join(".gemini/.env");
    let settings = dir.path().join(".gemini/settings.json");
    let original_env = concat!(
        "# keep this comment\r\n",
        "GOOGLE_API_KEY=legacy-key\r\n",
        "OTHER=value\r\n"
    );
    let original_settings = concat!(
        "{\r\n",
        "  // auth comment\r\n",
        "  \"security\": {\"auth\": {\"selectedType\": \"oauth-personal\"}}\r\n",
        "}\r\n"
    );
    common::write(&env_path, original_env);
    common::write(&settings, original_settings);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&GeminiSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert!(common::read(&env_path).contains("# keep this comment"));
    assert!(common::read(&settings).contains("// auth comment"));

    let report = wiring.disconnect(&GeminiSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&env_path).unwrap(), original_env.as_bytes());
    assert_eq!(fs::read(&settings).unwrap(), original_settings.as_bytes());
}

#[test]
fn user_edit_survives_disconnect() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".gemini/settings.json");
    common::write(
        &settings,
        "{\"security\": {\"auth\": {\"selectedType\": \"oauth-personal\"}}}",
    );
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());
    wiring
        .connect(&GeminiSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&settings);
    let edited = agent_wiring::edit::json::set(
        &text,
        &[agent_wiring::edit::KV::new(
            "theme",
            serde_json::json!("dark"),
        )],
    )
    .unwrap();
    common::write(&settings, &edited);

    let report = wiring.disconnect(&GeminiSpec, &ctx).unwrap();
    let text = common::read(&settings);
    assert_eq!(
        agent_wiring::edit::json::get(&text, "theme").as_deref(),
        Some("dark")
    );
    assert_eq!(report.kept_backup, vec![settings]);
}
