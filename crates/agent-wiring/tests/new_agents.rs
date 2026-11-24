#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::Path;

use agent_wiring::spec::agy::AgySpec;
use agent_wiring::spec::antigravity::AntigravitySpec;
use agent_wiring::spec::cursor::CursorSpec;
use agent_wiring::spec::hermes::HermesSpec;
use agent_wiring::spec::opencode::OpenCodeSpec;
use agent_wiring::spec::vscode::VSCodeSpec;
use agent_wiring::spec::{AgentSpec, HomeCtx, WiredState, WiringCtx};
use agent_wiring::wiring::Wiring;
use tempfile::tempdir;

fn ctx_for(home: &Path, env: &[(&str, String)]) -> HomeCtx {
    common::home_with(home, env)
}

fn wiring_ctx<'a>(catalog: &'a agent_wiring::ModelCatalog, model: &'a str) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: model,
        effort: None,
        roles: &common::NO_ROLES,
    }
}

/// Connects, asserts the wiring is visible to the agent, then disconnects and
/// asserts the pre-connect bytes came back.
///
/// Agents differ in where the gateway and key live (`agy` passes them through
/// its launcher, `cursor` only pins a model, `vscode` keeps the endpoint in
/// `chatLanguageModels.json`), so each test supplies its own `check`.
fn roundtrip(
    spec: &dyn AgentSpec,
    ctx: &HomeCtx,
    data: &Path,
    catalog: &agent_wiring::ModelCatalog,
    file: &Path,
    seed: &str,
    check: impl Fn(),
) {
    common::write(file, seed);
    let before = fs::read(file).unwrap();

    let wiring = Wiring::new(data);
    let wiring_ctx = wiring_ctx(catalog, common::DEFAULT_MODEL);
    wiring.connect(spec, ctx, &wiring_ctx).unwrap();

    check();
    assert_eq!(spec.read_state(ctx, common::GATEWAY), WiredState::Connected);

    let report = wiring.disconnect(spec, ctx).unwrap();
    assert!(
        report.kept_backup.is_empty(),
        "{}: kept {:?}, file now:\n{}",
        wire_id(spec),
        report.kept_backup,
        fs::read_to_string(file).unwrap_or_default()
    );
    assert_eq!(fs::read(file).unwrap(), before, "{}", wire_id(spec));
    assert_eq!(
        spec.read_state(ctx, common::GATEWAY),
        WiredState::Disconnected,
        "{}",
        wire_id(spec)
    );
}

fn wire_id(spec: &dyn AgentSpec) -> &'static str {
    spec.id()
}

#[test]
fn opencode_roundtrip_restores_user_json() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = ctx_for(home.path(), &[]);
    let catalog = common::catalog();
    let file = home.path().join(".config/opencode/opencode.json");
    roundtrip(
        &OpenCodeSpec,
        &ctx,
        data.path(),
        &catalog,
        &file,
        "{\n  \"theme\": \"system\"\n}\n",
        || {
            let text = common::read(&file);
            assert!(text.contains("\"provider\""), "{text}");
            // 1.x rejects unknown top-level keys, so a v1 file must NOT gain
            // a `providers` block: only files that already have one get it
            // refreshed.
            assert!(!text.contains("\"providers\""), "{text}");
            assert!(text.contains("any-converter/claude-sonnet-4-5"), "{text}");
            assert!(text.contains(common::GATEWAY), "{text}");
            assert!(text.contains(common::AGENT_KEY), "{text}");
        },
    );
}

#[test]
fn opencode_refreshes_an_existing_v2_block_in_its_own_shape() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = ctx_for(home.path(), &[]);
    let catalog = common::catalog();
    let file = home.path().join(".config/opencode/opencode.json");
    // A 2.x-native file already keeps our provider under `providers`; the
    // connect refreshes it there (with settings, not options) and drops the
    // 1.x entry, as OpenCode 2 itself does on load.
    let seed = "{\n  \"providers\": {\n    \"any-converter\": {\n      \"settings\": {\n        \"baseURL\": \"http://old\"\n      }\n    }\n  }\n}\n";
    common::write(&file, seed);
    let before = fs::read(&file).unwrap();

    let wiring = Wiring::new(data.path());
    wiring
        .connect(
            &OpenCodeSpec,
            &ctx,
            &wiring_ctx(&catalog, common::DEFAULT_MODEL),
        )
        .unwrap();
    let text = common::read(&file);
    assert!(text.contains("\"providers\""), "{text}");
    assert!(text.contains("\"settings\""), "{text}");
    assert!(text.contains("\"compatibility\""), "{text}");
    assert!(!text.contains("\"provider\""), "{text}");
    assert_eq!(
        OpenCodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    wiring.disconnect(&OpenCodeSpec, &ctx).unwrap();
    assert_eq!(fs::read(&file).unwrap(), before);
    // The user's pre-connect block (pointing at http://old) is restored, so
    // the state reader honestly reports drift: a provider named like ours
    // that no longer points at the gateway.
    assert_eq!(
        OpenCodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Drifted
    );
}

#[test]
fn vscode_roundtrip_restores_settings_and_keeps_other_groups() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let app_data = home.path().join("appdata");
    // VS Code's `User/` dir follows the platform's app-data convention:
    // `%APPDATA%` on Windows and `$XDG_CONFIG_HOME` on Linux (both overridden
    // here), `~/Library/Application Support` on macOS.
    let ctx = ctx_for(
        home.path(),
        &[
            ("APPDATA", app_data.display().to_string()),
            ("XDG_CONFIG_HOME", app_data.display().to_string()),
        ],
    );
    let catalog = common::catalog();
    let dir = if cfg!(target_os = "macos") {
        home.path().join("Library/Application Support/Code/User")
    } else {
        app_data.join("Code/User")
    };
    let settings = dir.join("settings.json");
    let models = dir.join("chatLanguageModels.json");
    common::write(
        &models,
        "{\"models\":[{\"name\":\"Other\",\"vendor\":\"customoai\"}]}",
    );
    roundtrip(
        &VSCodeSpec,
        &ctx,
        data.path(),
        &catalog,
        &settings,
        "{\n  \"editor.fontSize\": 13\n}\n",
        || {
            // The endpoint group lives beside settings.json, not inside it.
            let settings_text = common::read(&settings);
            assert!(
                settings_text.contains("\"chat.defaultModel\": \"claude-sonnet-4-5\""),
                "{settings_text}"
            );
            let models_text = common::read(&models);
            assert!(models_text.contains(common::GATEWAY), "{models_text}");
            assert!(models_text.contains(common::AGENT_KEY), "{models_text}");
            assert!(models_text.contains("Other"), "{models_text}");
        },
    );

    // The user's own group survives connect and disconnect.
    let kept = common::read(&models);
    assert!(kept.contains("Other"), "{kept}");
    assert!(!kept.contains("Any Converter"), "{kept}");
}

#[test]
fn antigravity_roundtrip_restores_gateway_block() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = ctx_for(home.path(), &[]);
    let catalog = common::catalog();
    let file = home.path().join(".gemini/config/config.json");
    roundtrip(
        &AntigravitySpec,
        &ctx,
        data.path(),
        &catalog,
        &file,
        "{\n  \"telemetry\": false\n}\n",
        || {
            let text = common::read(&file);
            assert!(text.contains(common::GATEWAY), "{text}");
            assert!(text.contains(common::AGENT_KEY), "{text}");
            // A reasoning model is listed once per level, never by its bare
            // id; medium is the safe default.
            assert!(
                text.contains("\"defaultModelId\": \"claude-sonnet-4-5:medium\""),
                "{text}"
            );
        },
    );
}

#[test]
fn antigravity_non_reasoning_model_is_written_bare() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = ctx_for(home.path(), &[]);
    let catalog = common::catalog();
    let file = home.path().join(".gemini/config/config.json");
    common::write(&file, "{}");

    let wiring = Wiring::new(data.path());
    wiring
        .connect(
            &AntigravitySpec,
            &ctx,
            &wiring_ctx(&catalog, "claude-haiku-4-5"),
        )
        .unwrap();
    let text = common::read(&file);
    assert!(
        text.contains("\"defaultModelId\": \"claude-haiku-4-5\""),
        "{text}"
    );
}

#[test]
fn vscode_custom_endpoint_uses_context_window() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let app_data = home.path().join("appdata");
    // VS Code's `User/` dir follows the platform's app-data convention:
    // `%APPDATA%` on Windows and `$XDG_CONFIG_HOME` on Linux (both overridden
    // here), `~/Library/Application Support` on macOS.
    let ctx = ctx_for(
        home.path(),
        &[
            ("APPDATA", app_data.display().to_string()),
            ("XDG_CONFIG_HOME", app_data.display().to_string()),
        ],
    );
    let catalog = common::catalog();
    let dir = if cfg!(target_os = "macos") {
        home.path().join("Library/Application Support/Code/User")
    } else {
        app_data.join("Code/User")
    };
    common::write(&dir.join("settings.json"), "{}");

    let wiring = Wiring::new(data.path());
    wiring
        .connect(
            &VSCodeSpec,
            &ctx,
            &wiring_ctx(&catalog, common::DEFAULT_MODEL),
        )
        .unwrap();

    let models: serde_json::Value =
        serde_json::from_str(&common::read(&dir.join("chatLanguageModels.json"))).unwrap();
    let group = models["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["name"] == "Any Converter")
        .unwrap();
    assert_eq!(group["vendor"], "customendpoint");
    let model = group["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "claude-sonnet-4-5")
        .unwrap();
    assert_eq!(model["contextWindow"], 200_000);
    assert_eq!(model["maxOutputTokens"], 16_384);
    // Not a customendpoint field.
    assert!(model.get("maxInputTokens").is_none(), "{model}");
    assert_eq!(model["vision"], false);
}

#[test]
fn vscode_vscodium_counts_input_as_window_minus_output() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let app_data = home.path().join("appdata");
    let ctx = ctx_for(
        home.path(),
        &[
            ("APPDATA", app_data.display().to_string()),
            ("XDG_CONFIG_HOME", app_data.display().to_string()),
        ],
    );
    let catalog = common::catalog();
    let dir = if cfg!(target_os = "macos") {
        home.path()
            .join("Library/Application Support/VSCodium/User")
    } else {
        app_data.join("VSCodium/User")
    };
    common::write(&dir.join("settings.json"), "{}");

    let wiring = Wiring::new(data.path());
    wiring
        .connect(
            &VSCodeSpec,
            &ctx,
            &wiring_ctx(&catalog, common::DEFAULT_MODEL),
        )
        .unwrap();

    let models: serde_json::Value =
        serde_json::from_str(&common::read(&dir.join("chatLanguageModels.json"))).unwrap();
    let group = &models["models"][0];
    assert_eq!(group["vendor"], "customoai");
    let model = group["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "claude-sonnet-4-5")
        .unwrap();
    assert_eq!(model["maxOutputTokens"], 16_384);
    assert_eq!(model["maxInputTokens"], 200_000 - 16_384);
    // The customoai vendor has no contextWindow.
    assert!(model.get("contextWindow").is_none(), "{model}");
}

#[test]
fn agy_roundtrip_restores_settings_and_removes_override() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = ctx_for(home.path(), &[]);
    let catalog = common::catalog();
    let settings = home.path().join(".gemini/antigravity-cli/settings.json");
    let override_path = home
        .path()
        .join(".gemini/antigravity-cli/any-converter-settings.json");
    roundtrip(
        &AgySpec,
        &ctx,
        data.path(),
        &catalog,
        &settings,
        "{\n  \"model\": \"gemini-2.5-pro\"\n}\n",
        || {
            // `agy` takes the endpoint and key from the environment, so the
            // user's own settings stay untouched and our values go to the
            // launcher-only override file.
            assert_eq!(
                common::read(&settings),
                "{\n  \"model\": \"gemini-2.5-pro\"\n}\n"
            );
            let text = common::read(&override_path);
            assert!(text.contains("\"modelProvider\": \"gemini\""), "{text}");
            assert!(text.contains("any-converter/claude-sonnet-4-5"), "{text}");
        },
    );

    assert!(!override_path.exists());
    assert_eq!(
        common::read(&settings),
        "{\n  \"model\": \"gemini-2.5-pro\"\n}\n"
    );
}

#[test]
fn agy_launcher_exports_gateway_and_key() {
    let home = tempdir().unwrap();
    let ctx = ctx_for(home.path(), &[]);
    let launch = AgySpec
        .launch(
            &ctx,
            common::GATEWAY,
            common::AGENT_KEY,
            common::DEFAULT_MODEL,
        )
        .expect("agy needs a launcher");
    assert!(launch.contains(common::AGENT_KEY));
    assert!(launch.contains(common::GATEWAY));
    assert!(launch.contains("--settings"));
    assert!(launch.contains("any-converter/claude-sonnet-4-5"));
}

#[test]
fn cursor_roundtrip_restores_model_block() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = ctx_for(home.path(), &[]);
    let catalog = common::catalog();
    let file = home.path().join(".cursor/cli-config.json");
    roundtrip(
        &CursorSpec,
        &ctx,
        data.path(),
        &catalog,
        &file,
        "{\n  \"model\": { \"modelId\": \"gpt-5\" }\n}\n",
        || {
            // Cursor CLI has no base-URL setting; we only pin the model.
            let text = common::read(&file);
            assert!(
                text.contains("\"modelId\": \"claude-sonnet-4-5\""),
                "{text}"
            );
            assert!(text.contains("\"hasChangedDefaultModel\": true"), "{text}");
        },
    );
}

#[test]
fn cursor_restores_the_changed_flag_as_a_boolean() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = ctx_for(home.path(), &[]);
    let catalog = common::catalog();
    let file = home.path().join(".cursor/cli-config.json");
    common::write(&file, "{\n  \"hasChangedDefaultModel\": false\n}\n");

    let wiring = Wiring::new(data.path());
    wiring
        .connect(
            &CursorSpec,
            &ctx,
            &wiring_ctx(&catalog, common::DEFAULT_MODEL),
        )
        .unwrap();
    let connected: serde_json::Value = serde_json::from_str(&common::read(&file)).unwrap();
    assert_eq!(connected["hasChangedDefaultModel"], true);

    wiring.disconnect(&CursorSpec, &ctx).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&common::read(&file)).unwrap();
    // A string "false" would neither parse nor mean what the CLI expects.
    assert_eq!(
        parsed["hasChangedDefaultModel"],
        serde_json::Value::Bool(false),
        "{parsed}"
    );
}

#[test]
fn hermes_roundtrip_restores_yaml() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let root = home.path().join("hermes");
    let ctx = ctx_for(home.path(), &[("HERMES_HOME", root.display().to_string())]);
    let catalog = common::catalog();
    let file = root.join("config.yaml");
    roundtrip(
        &HermesSpec,
        &ctx,
        data.path(),
        &catalog,
        &file,
        "# personal\nmodel:\n  provider: openai\n  default: gpt-5\n",
        || {
            // Connect keeps the comment, adds the provider, points the model
            // at it.
            let text = common::read(&file);
            assert!(text.contains("# personal"), "{text}");
            assert!(text.contains(common::GATEWAY), "{text}");
            assert!(text.contains(common::AGENT_KEY), "{text}");
            assert!(text.contains("claude-sonnet-4-5"), "{text}");
        },
    );

    assert_eq!(
        common::read(&file),
        "# personal\nmodel:\n  provider: openai\n  default: gpt-5\n"
    );
}
