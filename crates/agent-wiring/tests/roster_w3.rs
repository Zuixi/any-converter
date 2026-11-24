#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::Path;

use agent_wiring::edit::json;
use agent_wiring::spec::ante::AnteSpec;
use agent_wiring::spec::commandcode::CommandCodeSpec;
use agent_wiring::spec::copilot::CopilotSpec;
use agent_wiring::spec::copilot_jb::CopilotJetBrainsSpec;
use agent_wiring::spec::empryo::EmpryoSpec;
use agent_wiring::spec::fx::FxSpec;
use agent_wiring::spec::miniq::MiniqSpec;
use agent_wiring::spec::muse::MuseSpec;
use agent_wiring::spec::nanobot::NanobotSpec;
use agent_wiring::spec::paimon::PaimonSpec;
use agent_wiring::spec::qoder::{QoderSpec, qoder, qoder_cn};
use agent_wiring::spec::{AgentSpec, HomeCtx, WiredState, WiringCtx};
use agent_wiring::wiring::Wiring;
use agent_wiring::{CatalogModel, ModelCatalog};
use tempfile::tempdir;

fn wiring_ctx<'a>(catalog: &'a ModelCatalog, effort: Option<&'a str>) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: common::DEFAULT_MODEL,
        effort,
        roles: &common::NO_ROLES,
    }
}

/// Connects, runs `check`, then disconnects and asserts every covered file came
/// back byte-for-byte (or stayed absent when it started absent).
fn roundtrip(
    spec: &dyn AgentSpec,
    ctx: &HomeCtx,
    data: &Path,
    catalog: &ModelCatalog,
    files: &[&Path],
    effort: Option<&str>,
    check: impl Fn(),
) {
    let before: Vec<Option<Vec<u8>>> = files.iter().map(|file| fs::read(file).ok()).collect();

    let wiring = Wiring::new(data);
    let report = wiring
        .connect(spec, ctx, &wiring_ctx(catalog, effort))
        .unwrap_or_else(|error| panic!("{} connect failed: {error}", spec.id()));
    assert!(!report.files_written.is_empty(), "{}", spec.id());

    check();
    assert_eq!(
        spec.read_state(ctx, common::GATEWAY),
        WiredState::Connected,
        "{} should read as connected",
        spec.id()
    );

    let report = wiring
        .disconnect(spec, ctx)
        .unwrap_or_else(|error| panic!("{} disconnect failed: {error}", spec.id()));
    assert!(
        report.kept_backup.is_empty(),
        "{} kept backups: {:?}",
        spec.id(),
        report.kept_backup
    );
    for (file, bytes) in files.iter().zip(&before) {
        assert_eq!(
            &fs::read(file).ok(),
            bytes,
            "{} restored {}",
            spec.id(),
            file.display()
        );
    }
    assert_eq!(
        spec.read_state(ctx, common::GATEWAY),
        WiredState::Disconnected,
        "{} should read as disconnected",
        spec.id()
    );
}

#[test]
fn commandcode_roundtrip_keeps_user_efforts() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let providers = home.path().join(".commandcode/providers.json");
    let settings = home.path().join(".commandcode/settings.json");
    common::write(
        &providers,
        "{\n  \"provider\": {\n    \"openai\": { \"name\": \"OpenAI\" }\n  },\n  \"ui\": { \"theme\": \"dark\" }\n}\n",
    );
    common::write(
        &settings,
        "{\n  \"model\": \"openai/gpt-5\",\n  \"modelProvider\": \"openai\",\n  \"reasoningEffort\": { \"openai/gpt-5\": \"low\" }\n}\n",
    );

    roundtrip(
        &CommandCodeSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&providers, &settings],
        Some("high"),
        || {
            let providers_text = common::read(&providers);
            assert_eq!(
                json::get(&providers_text, "provider.any-converter.name").as_deref(),
                Some("any-converter")
            );
            assert_eq!(
                json::get(&providers_text, "provider.any-converter.api").as_deref(),
                Some("openai-completions")
            );
            assert_eq!(
                json::get(&providers_text, "provider.any-converter.apiKey").as_deref(),
                Some("false")
            );
            assert_eq!(
                json::get(&providers_text, "provider.any-converter.baseURL").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(
                json::get(
                    &providers_text,
                    "provider.any-converter.models.claude-sonnet-4-5.contextWindow"
                )
                .as_deref(),
                Some("200000")
            );
            let efforts = json::get(
                &providers_text,
                "provider.any-converter.models.claude-sonnet-4-5.reasoningEfforts",
            )
            .unwrap();
            assert!(efforts.contains("xhigh"), "{efforts}");
            assert_eq!(
                json::get(
                    &providers_text,
                    "provider.any-converter.models.claude-sonnet-4-5.reasoning"
                )
                .as_deref(),
                Some("true")
            );
            // The catalog marks Haiku as non-reasoning: no reasoning keys.
            assert_eq!(
                json::get(
                    &providers_text,
                    "provider.any-converter.models.claude-haiku-4-5.reasoning"
                ),
                None
            );
            assert_eq!(
                json::get(
                    &providers_text,
                    "provider.any-converter.models.claude-haiku-4-5.reasoningEfforts"
                ),
                None
            );

            let settings_text = common::read(&settings);
            assert_eq!(
                json::get(&settings_text, "model").as_deref(),
                Some("any-converter/claude-sonnet-4-5")
            );
            assert_eq!(
                json::get(&settings_text, "modelProvider").as_deref(),
                Some("any-converter")
            );
            assert_eq!(
                json::get(
                    &settings_text,
                    "reasoningEffort.any-converter/claude-sonnet-4-5"
                )
                .as_deref(),
                Some("high")
            );
            assert_eq!(
                json::get(&settings_text, "reasoningEffort.openai/gpt-5").as_deref(),
                Some("low")
            );
        },
    );
}

#[test]
fn fx_roundtrip_restores_keyless_provider() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let settings = home.path().join(".fx/settings.json");
    common::write(
        &settings,
        "{\n  \"provider\": \"anthropic\",\n  \"ui\": { \"theme\": \"dark\" }\n}\n",
    );

    roundtrip(
        &FxSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&settings],
        None,
        || {
            let text = common::read(&settings);
            assert_eq!(
                json::get(&text, "providers.any-converter.protocol").as_deref(),
                Some("openai-chat-completions")
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.base_url").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.auth.type").as_deref(),
                Some("none")
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.tool_choice_mode").as_deref(),
                Some("send")
            );
            assert_eq!(
                json::get(
                    &text,
                    "providers.any-converter.model_metadata.claude-sonnet-4-5.context_window"
                )
                .as_deref(),
                Some("200000")
            );
            assert_eq!(
                json::get(
                    &text,
                    "providers.any-converter.model_metadata.claude-sonnet-4-5.max_output_tokens"
                ),
                None
            );
            assert_eq!(
                json::get(&text, "provider").as_deref(),
                Some("any-converter")
            );
            // fx composes `<provider>/<ref>` itself: the ref must stay bare.
            assert_eq!(
                json::get(&text, "models.any-converter").as_deref(),
                Some("claude-sonnet-4-5")
            );
            assert_eq!(json::get(&text, "ui.theme").as_deref(), Some("dark"));
        },
    );
}

#[test]
fn fx_caps_model_metadata_at_256_entries() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let models: Vec<CatalogModel> = (0..257)
        .map(|index| {
            CatalogModel::new(
                format!("model-{index}"),
                format!("Model {index}"),
                false,
                Some(8_192),
            )
        })
        .collect();
    let catalog = ModelCatalog::new(models);
    let settings = home.path().join(".fx/settings.json");
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&FxSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&settings);
    let metadata: serde_json::Value =
        serde_json::from_str(&json::get(&text, "providers.any-converter.model_metadata").unwrap())
            .unwrap();
    let entries = metadata.as_object().unwrap();
    assert_eq!(entries.len(), 256);
    assert!(entries.contains_key("model-0"));
    assert!(!entries.contains_key("model-256"));
}

#[test]
fn empryo_roundtrip_keeps_user_providers() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let config = home.path().join(".empryo/config.json");
    common::write(
        &config,
        "{\n  \"providers\": [{ \"id\": \"openai\", \"name\": \"OpenAI\" }],\n  \"defaultModel\": \"openai/gpt-5\",\n  \"theme\": \"dark\"\n}\n",
    );

    roundtrip(
        &EmpryoSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&config],
        None,
        || {
            let text = common::read(&config);
            let providers = json::get(&text, "providers").unwrap();
            assert!(
                providers.contains("\"id\":\"any-converter\""),
                "{providers}"
            );
            assert!(providers.contains("openai"), "{providers}");
            assert_eq!(
                json::get(&text, "defaultModel").as_deref(),
                Some("any-converter/claude-sonnet-4-5")
            );
            assert_eq!(json::get(&text, "theme").as_deref(), Some("dark"));
        },
    );
}

#[test]
fn nanobot_roundtrip_restores_agent_defaults() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let config = home.path().join(".nanobot/config.json");
    common::write(
        &config,
        "{\n  \"agents\": {\n    \"defaults\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-opus-4-5\",\n      \"modelPreset\": \"work\",\n      \"model_preset\": \"snake-work\",\n      \"provider_name\": \"legacy-provider\",\n      \"model_name\": \"legacy-model\",\n      \"contextWindowTokens\": 64000,\n      \"context_window_tokens\": 32000,\n      \"temperature\": 0.2\n    }\n  },\n  \"theme\": \"dark\"\n}\n",
    );

    roundtrip(
        &NanobotSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&config],
        None,
        || {
            let text = common::read(&config);
            assert_eq!(
                json::get(&text, "providers.any-converter.apiKey").as_deref(),
                Some(common::AGENT_KEY)
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.apiBase").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.extraHeaders.User-Agent").as_deref(),
                Some("nanobot")
            );
            assert_eq!(
                json::get(&text, "agents.defaults.provider").as_deref(),
                Some("any-converter")
            );
            assert_eq!(
                json::get(&text, "agents.defaults.model").as_deref(),
                Some("any-converter/claude-sonnet-4-5")
            );
            assert_eq!(
                json::get(&text, "agents.defaults.contextWindowTokens").as_deref(),
                Some("200000")
            );
            // Presets override the pair we selected, so connect clears both.
            assert_eq!(json::get(&text, "agents.defaults.modelPreset"), None);
            assert_eq!(json::get(&text, "agents.defaults.model_preset"), None);
            // The snake_case window would shadow ours: it goes too.
            assert_eq!(
                json::get(&text, "agents.defaults.context_window_tokens"),
                None
            );
            // Phantom keys we never read must survive a connect untouched.
            assert_eq!(
                json::get(&text, "agents.defaults.provider_name").as_deref(),
                Some("legacy-provider")
            );
            assert_eq!(
                json::get(&text, "agents.defaults.model_name").as_deref(),
                Some("legacy-model")
            );
            assert_eq!(
                json::get(&text, "agents.defaults.temperature").as_deref(),
                Some("0.2")
            );
        },
    );

    // The stashed presets are restored byte-for-byte by `roundtrip` above.
    let restored = common::read(&config);
    assert_eq!(
        json::get(&restored, "agents.defaults.modelPreset").as_deref(),
        Some("work")
    );
    assert_eq!(
        json::get(&restored, "agents.defaults.model_preset").as_deref(),
        Some("snake-work")
    );
    // Numeric values must come back as numbers, not quoted strings, in both
    // spellings the user had.
    let parsed: serde_json::Value = serde_json::from_str(&restored).unwrap();
    assert_eq!(
        parsed["agents"]["defaults"]["contextWindowTokens"].as_u64(),
        Some(64_000)
    );
    assert_eq!(
        parsed["agents"]["defaults"]["context_window_tokens"].as_u64(),
        Some(32_000)
    );
}

#[test]
fn miniq_roundtrip_restores_provider_object() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let data_dir = home.path().join("miniq-data");
    fs::create_dir_all(&data_dir).unwrap();
    let ctx = common::home_with(
        home.path(),
        &[("MINIQ_DATA_DIR", data_dir.display().to_string())],
    );
    let catalog = common::catalog();
    let settings = data_dir.join("settings.json");
    common::write(
        &settings,
        "{\n  \"provider\": {\n    \"baseUrl\": \"https://api.anthropic.com\",\n    \"apiKey\": \"sk-user\",\n    \"model\": \"claude-opus-4-5\",\n    \"apiProtocol\": \"anthropic\"\n  },\n  \"ui\": { \"theme\": \"dark\" }\n}\n",
    );

    roundtrip(
        &MiniqSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&settings],
        None,
        || {
            let text = common::read(&settings);
            assert_eq!(
                json::get(&text, "provider.baseUrl").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(
                json::get(&text, "provider.apiKey").as_deref(),
                Some(common::AGENT_KEY)
            );
            assert_eq!(
                json::get(&text, "provider.model").as_deref(),
                Some("claude-sonnet-4-5")
            );
            assert_eq!(
                json::get(&text, "provider.apiProtocol").as_deref(),
                Some("chat_completions")
            );
            assert_eq!(json::get(&text, "ui.theme").as_deref(), Some("dark"));
        },
    );
}

#[test]
fn paimon_roundtrip_restores_litellm_block() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let config_home = home.path().join("paimon-config");
    let ctx = common::home_with(
        home.path(),
        &[("PAIMON_CONFIG_HOME", config_home.display().to_string())],
    );
    let catalog = common::catalog();
    let config = config_home.join("config.json");
    common::write(
        &config,
        "{\n  \"model\": \"anthropic:claude-opus-4-5\",\n  \"providers\": { \"anthropic\": { \"api_key\": \"sk-user\" } },\n  \"compaction\": { \"context_window\": 100000, \"strategy\": \"auto\" }\n}\n",
    );

    roundtrip(
        &PaimonSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&config],
        None,
        || {
            let text = common::read(&config);
            assert_eq!(
                json::get(&text, "model").as_deref(),
                Some("litellm:claude-sonnet-4-5")
            );
            assert_eq!(
                json::get(&text, "providers.litellm.api_base").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(
                json::get(&text, "providers.litellm.api_key").as_deref(),
                Some(common::AGENT_KEY)
            );
            assert_eq!(
                json::get(&text, "compaction.context_window").as_deref(),
                Some("200000")
            );
            assert_eq!(
                json::get(&text, "compaction.strategy").as_deref(),
                Some("auto")
            );
        },
    );
}

#[test]
fn miniq_pointer_file_relocates_the_data_dir() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let local = home.path().join("localappdata");
    let ctx = common::home_with(
        home.path(),
        &[("LOCALAPPDATA", local.display().to_string())],
    );
    let catalog = common::catalog();
    // %LOCALAPPDATA% wins on every platform, so the default is its child.
    let default = local.join("miniq");
    let custom = home.path().join("custom-miniq-data");
    fs::create_dir_all(&default).unwrap();
    common::write(
        &default.with_file_name("miniq-data-dir.json"),
        &serde_json::json!({ "data_dir": custom.display().to_string() }).to_string(),
    );
    let settings = custom.join("settings.json");
    common::write(
        &settings,
        "{\n  \"provider\": { \"baseUrl\": \"https://api.anthropic.com\" },\n  \"ui\": { \"theme\": \"dark\" }\n}\n",
    );

    roundtrip(
        &MiniqSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&settings],
        None,
        || {
            let text = common::read(&settings);
            assert_eq!(
                json::get(&text, "provider.baseUrl").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(json::get(&text, "ui.theme").as_deref(), Some("dark"));
        },
    );
}

#[test]
fn paimon_ignores_a_relative_config_home_override() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let xdg = home.path().join("xdg");
    let ctx = common::home_with(
        home.path(),
        &[
            ("PAIMON_CONFIG_HOME", "relative-paimon".to_owned()),
            ("XDG_CONFIG_HOME", xdg.display().to_string()),
        ],
    );
    let catalog = common::catalog();
    // The relative override and XDG_CONFIG_HOME are both ignored.
    let config = home.path().join(".config/paimon/config.json");
    common::write(
        &config,
        "{\n  \"model\": \"anthropic:claude-opus-4-5\"\n}\n",
    );

    roundtrip(
        &PaimonSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&config],
        None,
        || {
            let text = common::read(&config);
            assert_eq!(
                json::get(&text, "model").as_deref(),
                Some("litellm:claude-sonnet-4-5")
            );
            assert_eq!(
                json::get(&text, "providers.litellm.api_base").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
        },
    );
}

#[test]
fn paimon_without_a_catalog_window_falls_back_to_the_user_window() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = ModelCatalog::new(vec![CatalogModel::new(
        common::DEFAULT_MODEL,
        "Claude Sonnet 4.5",
        true,
        None,
    )]);
    let config = home.path().join(".config/paimon/config.json");
    common::write(
        &config,
        "{\n  \"compaction\": { \"context_window\": 50000, \"strategy\": \"auto\" }\n}\n",
    );

    roundtrip(
        &PaimonSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&config],
        None,
        || {
            let text = common::read(&config);
            assert_eq!(
                json::get(&text, "compaction.context_window").as_deref(),
                Some("50000"),
                "a user window must survive a model with no catalog window"
            );
        },
    );
}

#[test]
fn paimon_deletes_window_when_no_user_window_is_known() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = ModelCatalog::new(vec![CatalogModel::new(
        common::DEFAULT_MODEL,
        "Claude Sonnet 4.5",
        true,
        None,
    )]);
    let config = home.path().join(".config/paimon/config.json");
    common::write(
        &config,
        "{\n  \"compaction\": { \"strategy\": \"auto\" }\n}\n",
    );
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&PaimonSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    let text = common::read(&config);
    assert_eq!(json::get(&text, "compaction.context_window"), None);
    assert_eq!(
        json::get(&text, "compaction.strategy").as_deref(),
        Some("auto")
    );
    wiring.disconnect(&PaimonSpec, &ctx).unwrap();
}

#[test]
fn ante_roundtrip_restores_catalog_and_settings() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let root = home.path().join("ante-home");
    let ctx = common::home_with(home.path(), &[("ANTE_HOME", root.display().to_string())]);
    let catalog = common::catalog();
    let catalog_file = root.join("catalog.json");
    let settings = root.join("settings.json");
    common::write(
        &catalog_file,
        "{\n  \"providers\": { \"anthropic\": { \"display_name\": \"Anthropic\" } },\n  \"version\": 2\n}\n",
    );
    common::write(
        &settings,
        "{\n  \"provider\": \"anthropic\",\n  \"model\": \"claude-opus-4-5\",\n  \"theme\": \"dark\"\n}\n",
    );

    roundtrip(
        &AnteSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&catalog_file, &settings],
        None,
        || {
            let catalog_text = common::read(&catalog_file);
            assert_eq!(
                json::get(&catalog_text, "providers.any-converter.display_name").as_deref(),
                Some("any-converter")
            );
            assert_eq!(
                json::get(&catalog_text, "providers.any-converter.base_url").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(
                json::get(&catalog_text, "providers.any-converter.wire_style").as_deref(),
                Some("OpenAiCompatible")
            );
            let models =
                json::get(&catalog_text, "providers.any-converter.preferred_models").unwrap();
            assert!(models.contains("\"id\":\"claude-sonnet-4-5\""), "{models}");
            // The display name is `display_name`, never `name`.
            assert!(
                models.contains("\"display_name\":\"Claude Sonnet 4.5\""),
                "{models}"
            );
            assert!(models.contains("\"context_limit\":200000"), "{models}");
            assert!(models.contains("\"max_tokens\":16384"), "{models}");
            // Generic "minimal" maps onto Ante's "min"; there is no "minimal".
            assert!(!models.contains("\"minimal\""), "{models}");
            assert!(
                models.contains("[\"min\",\"low\",\"medium\",\"high\",\"xhigh\",\"max\"]"),
                "{models}"
            );

            let parsed: serde_json::Value = serde_json::from_str(&models).unwrap();
            let haiku = parsed
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"].as_str() == Some("claude-haiku-4-5"))
                .unwrap();
            assert!(
                haiku.get("supported_efforts").is_none(),
                "non-reasoning models must omit efforts: {haiku}"
            );

            let settings_text = common::read(&settings);
            assert_eq!(
                json::get(&settings_text, "provider").as_deref(),
                Some("any-converter")
            );
            assert_eq!(
                json::get(&settings_text, "model").as_deref(),
                Some("claude-sonnet-4-5")
            );
            assert_eq!(json::get(&settings_text, "theme").as_deref(), Some("dark"));
        },
    );
}

#[test]
fn ante_disconnect_keeps_a_selection_the_user_changed() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let root = home.path().join("ante-home");
    let ctx = common::home_with(home.path(), &[("ANTE_HOME", root.display().to_string())]);
    let catalog = common::catalog();
    let settings = root.join("settings.json");
    common::write(
        &settings,
        "{\n  \"provider\": \"openai\",\n  \"model\": \"gpt-5\"\n}\n",
    );
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&AnteSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    assert_eq!(
        json::get(&common::read(&settings), "provider").as_deref(),
        Some("any-converter")
    );

    // The user points Ante at another provider while we are connected.
    common::write(
        &settings,
        "{\n  \"provider\": \"anthropic\",\n  \"model\": \"claude-opus-4-5\"\n}\n",
    );
    wiring.disconnect(&AnteSpec, &ctx).unwrap();

    let text = common::read(&settings);
    assert_eq!(
        json::get(&text, "provider").as_deref(),
        Some("anthropic"),
        "disconnect must not clobber a foreign selection"
    );
    assert_eq!(
        json::get(&text, "model").as_deref(),
        Some("claude-opus-4-5")
    );
}

#[test]
fn muse_roundtrip_restores_transport() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let settings = home.path().join(".config/muse/settings.json");
    common::write(
        &settings,
        "{\n  \"schema_version\": 1,\n  \"ui\": { \"theme\": \"dark\" }\n}\n",
    );

    roundtrip(
        &MuseSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&settings],
        None,
        || {
            let text = common::read(&settings);
            assert_eq!(json::get(&text, "schema_version").as_deref(), Some("1"));
            assert_eq!(
                json::get(&text, "endpoint_transport.base_url").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(
                json::get(&text, "endpoint_transport.auth").as_deref(),
                Some("none")
            );
            assert_eq!(
                json::get(&text, "model").as_deref(),
                Some("claude-sonnet-4-5")
            );
            assert_eq!(json::get(&text, "ui.theme").as_deref(), Some("dark"));
        },
    );
}

#[test]
fn muse_leaves_schema_version_when_it_created_the_file() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let settings = home.path().join(".config/muse/settings.json");
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&MuseSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    assert!(settings.exists());
    wiring.disconnect(&MuseSpec, &ctx).unwrap();
    assert!(settings.exists());
    let text = common::read(&settings);
    assert_eq!(json::get(&text, "schema_version").as_deref(), Some("1"));
    assert_eq!(json::get(&text, "model"), None);
    assert_eq!(json::get(&text, "endpoint_transport"), None);
}

#[test]
fn muse_does_not_downgrade_a_newer_schema_version() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let settings = home.path().join(".config/muse/settings.json");
    common::write(&settings, "{\n  \"schema_version\": 7\n}\n");
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&MuseSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    let connected = common::read(&settings);
    assert_eq!(
        json::get(&connected, "schema_version").as_deref(),
        Some("7")
    );
    assert_eq!(
        json::get(&connected, "endpoint_transport.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );

    wiring.disconnect(&MuseSpec, &ctx).unwrap();
    assert_eq!(
        json::get(&common::read(&settings), "schema_version").as_deref(),
        Some("7")
    );
}

fn qoder_case(spec: QoderSpec, ctx: &HomeCtx, dir: &Path, data: &Path, catalog: &ModelCatalog) {
    let settings = dir.join("settings.json");
    common::write(
        &settings,
        "{\n  \"model\": { \"name\": \"anthropic/claude-opus-4-5\", \"temperature\": 0.1 },\n  \"providers\": { \"anthropic\": {} },\n  \"theme\": \"dark\"\n}\n",
    );

    roundtrip(
        &spec,
        ctx,
        data,
        catalog,
        &[&settings],
        Some("high"),
        || {
            let text = common::read(&settings);
            assert_eq!(
                json::get(&text, "providers.any-converter.displayName").as_deref(),
                Some("any-converter")
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.protocol").as_deref(),
                Some("openai")
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.baseUrl").as_deref(),
                Some("http://127.0.0.1:8787/v1")
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.apiKey").as_deref(),
                Some(common::AGENT_KEY)
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.model").as_deref(),
                Some(common::DEFAULT_MODEL)
            );
            let models = json::get(&text, "providers.any-converter.models").unwrap();
            assert!(models.contains("\"capabilities\""), "{models}");
            // Catalog order: opus, sonnet, haiku, ...
            assert_eq!(
                json::get(
                    &text,
                    "providers.any-converter.models.0.capabilities.thinking.supportedEffortLevels"
                )
                .as_deref(),
                Some("[\"low\",\"medium\",\"high\",\"xhigh\",\"max\"]")
            );
            // Non-reasoning Haiku has no thinking capabilities and no output limit.
            assert_eq!(
                json::get(
                    &text,
                    "providers.any-converter.models.2.capabilities.thinking"
                ),
                None
            );
            assert_eq!(
                json::get(&text, "providers.any-converter.models.2.maxOutputTokens"),
                None
            );
            assert_eq!(
                json::get(&text, "model.name").as_deref(),
                Some("any-converter/claude-sonnet-4-5")
            );
            assert_eq!(
                json::get(&text, "model.reasoningEffort").as_deref(),
                Some("high")
            );
            assert_eq!(
                json::get(
                    &text,
                    "model.preferences.any-converter/claude-sonnet-4-5.reasoning.effort"
                )
                .as_deref(),
                Some("high")
            );
            assert_eq!(
                json::get(&text, "model.temperature").as_deref(),
                Some("0.1")
            );
        },
    );
}

#[test]
fn qoder_roundtrip_restores_settings() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    qoder_case(
        qoder(),
        &ctx,
        &home.path().join(".qoder"),
        data.path(),
        &catalog,
    );
}

#[test]
fn qoder_cn_roundtrip_uses_env_dir() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let dir = home.path().join("qoder-cn-custom");
    fs::create_dir_all(&dir).unwrap();
    let ctx = common::home_with(
        home.path(),
        &[("QODERCN_CONFIG_DIR", dir.display().to_string())],
    );
    let catalog = common::catalog();
    qoder_case(qoder_cn(), &ctx, &dir, data.path(), &catalog);
}

#[test]
fn copilot_roundtrip_preserves_unknown_keys() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let providers_override = home.path().join("copilot-providers/providers.json");
    let ctx = common::home_with(
        home.path(),
        &[(
            "COPILOT_PROVIDERS_CONFIG",
            providers_override.display().to_string(),
        )],
    );
    let catalog = common::catalog();
    let settings = home.path().join(".copilot/settings.json");
    common::write(
        &providers_override,
        "{\n  \"providers\": [{ \"name\": \"user-openai\", \"type\": \"openai\", \"baseUrl\": \"https://api.openai.com/v1\", \"apiKey\": \"sk-user\" }],\n  \"models\": [{ \"id\": \"gpt-5\", \"provider\": \"user-openai\", \"name\": \"GPT-5\" }],\n  \"unknownTopLevel\": { \"keep\": true }\n}\n",
    );
    common::write(
        &settings,
        "{\n  \"model\": \"user-openai/gpt-5\",\n  \"theme\": \"dark\"\n}\n",
    );

    roundtrip(
        &CopilotSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&providers_override, &settings],
        Some("high"),
        || {
            let providers_text = common::read(&providers_override);
            let providers = json::get(&providers_text, "providers").unwrap();
            assert!(
                providers.contains("\"name\":\"any-converter\""),
                "{providers}"
            );
            assert!(providers.contains("user-openai"), "{providers}");
            let models = json::get(&providers_text, "models").unwrap();
            assert!(
                models.contains("\"provider\":\"any-converter\""),
                "{models}"
            );
            assert!(models.contains("\"id\":\"gpt-5\""), "{models}");
            assert_eq!(
                json::get(&providers_text, "unknownTopLevel.keep").as_deref(),
                Some("true")
            );

            let settings_text = common::read(&settings);
            assert_eq!(
                json::get(&settings_text, "model").as_deref(),
                Some("any-converter/claude-sonnet-4-5")
            );
            assert_eq!(
                json::get(&settings_text, "effortLevel").as_deref(),
                Some("high")
            );
            assert_eq!(json::get(&settings_text, "theme").as_deref(), Some("dark"));
        },
    );
}

#[test]
fn copilot_omits_a_name_redundant_with_the_model_id() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = ModelCatalog::new(vec![
        CatalogModel::new("same-model", "same-model", false, Some(8_192)),
        CatalogModel::new("other-model", "Other Model", false, Some(8_192)),
    ]);
    let providers = home.path().join(".copilot/providers.json");
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&CopilotSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&providers);
    assert_eq!(json::get(&text, "models.0.name"), None);
    assert_eq!(
        json::get(&text, "models.1.name").as_deref(),
        Some("Other Model")
    );
}

#[test]
fn copilot_jetbrains_preserves_existing_registration_state() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let xdg = home.path().join("xdg");
    let ctx = common::home_with(
        home.path(),
        &[("XDG_CONFIG_HOME", xdg.display().to_string())],
    );
    let catalog = common::catalog();
    let byok = xdg.join("github-copilot").join("byok.json");
    common::write(
        &byok,
        "{\n  \"user-setting\": true,\n  \"any-converter-models-config\": {\n    \"claude-sonnet-4-5\": { \"isRegistered\": false }\n  }\n}\n",
    );
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&CopilotJetBrainsSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    let text = common::read(&byok);
    // A pre-existing entry keeps its registration state.
    assert_eq!(
        json::get(
            &text,
            "any-converter-models-config.claude-sonnet-4-5.isRegistered"
        )
        .as_deref(),
        Some("false")
    );
    assert_eq!(
        json::get(
            &text,
            "any-converter-models-config.claude-haiku-4-5.isRegistered"
        )
        .as_deref(),
        Some("true")
    );
    assert_eq!(
        json::get(
            &text,
            "any-converter-models-config.claude-sonnet-4-5.modelCapabilities.maxInputTokens"
        )
        .as_deref(),
        Some("200000")
    );

    wiring.disconnect(&CopilotJetBrainsSpec, &ctx).unwrap();
    let text = common::read(&byok);
    assert_eq!(json::get(&text, "user-setting").as_deref(), Some("true"));
    assert_eq!(json::get(&text, "any-converter-provider-config"), None);
    assert_eq!(json::get(&text, "any-converter-models-config"), None);
}

#[test]
fn copilot_jetbrains_roundtrip_restores_byok() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let local = home.path().join("localappdata");
    let xdg = home.path().join("xdg");
    let ctx = common::home_with(
        home.path(),
        &[
            ("LOCALAPPDATA", local.display().to_string()),
            ("XDG_CONFIG_HOME", xdg.display().to_string()),
        ],
    );
    let catalog = common::catalog();
    // An absolute XDG_CONFIG_HOME wins on every platform, Windows included.
    let byok = xdg.join("github-copilot").join("byok.json");
    common::write(&byok, "{\n  \"user-setting\": true\n}\n");

    roundtrip(
        &CopilotJetBrainsSpec,
        &ctx,
        data.path(),
        &catalog,
        &[&byok],
        None,
        || {
            let text = common::read(&byok);
            assert_eq!(
                json::get(&text, "any-converter-provider-config.groupName").as_deref(),
                Some("any-converter")
            );
            assert_eq!(
                json::get(&text, "any-converter-provider-config.apiType").as_deref(),
                Some("chatCompletions")
            );
            assert_eq!(
                json::get(&text, "any-converter-api-key").as_deref(),
                Some(common::AGENT_KEY)
            );
            assert_eq!(
                json::get(
                    &text,
                    "any-converter-models-config.claude-sonnet-4-5.deploymentUrl"
                )
                .as_deref(),
                Some("http://127.0.0.1:8787/v1/chat/completions")
            );
            assert_eq!(
                json::get(
                    &text,
                    "any-converter-models-config.claude-sonnet-4-5.isRegistered"
                )
                .as_deref(),
                Some("true")
            );
            assert_eq!(
                json::get(
                    &text,
                    "any-converter-models-config.claude-sonnet-4-5.modelCapabilities.maxInputTokens"
                )
                .as_deref(),
                Some("200000")
            );
            assert_eq!(
                json::get(
                    &text,
                    "any-converter-models-config.claude-sonnet-4-5.modelCapabilities.maxOutputTokens"
                )
                .as_deref(),
                Some("16000")
            );
            assert_eq!(
                json::get(
                    &text,
                    "any-converter-models-config.claude-sonnet-4-5.modelCapabilities.toolCalling"
                )
                .as_deref(),
                Some("true")
            );
        },
    );
}
