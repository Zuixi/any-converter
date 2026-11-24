#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use agent_wiring::edit::{json, yaml};
use agent_wiring::spec::crush::CrushSpec;
use agent_wiring::spec::goose::GooseSpec;
use agent_wiring::spec::hermes::HermesSpec;
use agent_wiring::spec::openchamber::OpenChamberSpec;
use agent_wiring::spec::openclaw::OpenClawSpec;
use agent_wiring::spec::opencode::OpenCodeSpec;
use agent_wiring::spec::opencode_family::{deveco, mimocode};
use agent_wiring::spec::qwenpaw::QwenPawSpec;
use agent_wiring::spec::snow::SnowSpec;
use agent_wiring::spec::{AgentSpec, HomeCtx, WiredState, WiringCtx};
use agent_wiring::sync::catalog::{CatalogModel, ModelCatalog};
use agent_wiring::wiring::Wiring;
use serde_json::Value;
use tempfile::tempdir;

fn wiring_ctx<'a>(
    catalog: &'a agent_wiring::ModelCatalog,
    effort: Option<&'a str>,
) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: common::DEFAULT_MODEL,
        effort,
        roles: &common::NO_ROLES,
    }
}

fn parse(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap()
}

#[test]
fn opencode_family_roundtrip_and_shape() {
    let catalog = common::catalog();
    let mut provider_blocks = Vec::new();

    for spec in [mimocode(), deveco()] {
        let home = tempdir().unwrap();
        let data = tempdir().unwrap();
        let ctx = common::home(home.path());
        let file = home
            .path()
            .join(".config")
            .join(spec.dir_name)
            .join(format!("{}.json", spec.dir_name));
        let seed = concat!(
            "{\n",
            "  \"theme\": \"dark\",\n",
            "  \"model\": \"openai/gpt-5\",\n",
            "  \"small_model\": \"openai/gpt-5-mini\"\n",
            "}\n"
        );
        common::write(&file, seed);

        let wiring = Wiring::new(data.path());
        wiring
            .connect(&spec, &ctx, &wiring_ctx(&catalog, None))
            .unwrap();

        let text = common::read(&file);
        assert_eq!(
            json::get(&text, "provider.any-converter.name").as_deref(),
            Some("any-converter"),
            "{}",
            spec.id()
        );
        assert_eq!(
            json::get(&text, "provider.any-converter.options.baseURL").as_deref(),
            Some("http://127.0.0.1:8787/v1")
        );
        assert_eq!(
            json::get(&text, "provider.any-converter.options.apiKey").as_deref(),
            Some(common::AGENT_KEY)
        );
        assert_eq!(
            json::get(
                &text,
                "provider.any-converter.models.claude-sonnet-4-5.name"
            )
            .as_deref(),
            Some("Claude Sonnet 4.5")
        );
        assert_eq!(
            json::get(
                &text,
                "provider.any-converter.models.claude-sonnet-4-5.reasoning"
            )
            .as_deref(),
            Some("true")
        );
        // A non-reasoning catalog model gets no `reasoning` flag.
        assert_eq!(
            json::get(
                &text,
                "provider.any-converter.models.claude-haiku-4-5.reasoning"
            ),
            None
        );
        assert_eq!(
            json::get(&text, "model").as_deref(),
            Some("any-converter/claude-sonnet-4-5")
        );
        // The user's own small model is never overwritten on connect.
        assert_eq!(
            json::get(&text, "small_model").as_deref(),
            Some("openai/gpt-5-mini")
        );
        // The seed has no `providers` block, and a v1 config rejects unknown
        // top-level keys — so none may appear.
        assert_eq!(
            json::get(&text, "providers.any-converter.name").as_deref(),
            None,
            "{}",
            spec.id()
        );
        assert_eq!(
            spec.read_state(&ctx, common::GATEWAY),
            WiredState::Connected
        );
        provider_blocks.push(json::get(&text, "provider.any-converter").unwrap());

        let report = wiring.disconnect(&spec, &ctx).unwrap();
        assert!(report.kept_backup.is_empty(), "{}", spec.id());
        assert_eq!(fs::read(&file).unwrap(), seed.as_bytes(), "{}", spec.id());
        assert_eq!(
            spec.read_state(&ctx, common::GATEWAY),
            WiredState::Disconnected
        );
    }

    // The family members differ only by their id, directory, env var, and binary.
    assert_ne!(mimocode().id(), deveco().id());
    assert_ne!(mimocode().dir_name, deveco().dir_name);
    assert_ne!(mimocode().env_var, deveco().env_var);
    assert_ne!(mimocode().bin, deveco().bin);
    assert_eq!(mimocode().our_keys(), deveco().our_keys());
    assert_eq!(mimocode().efforts(), deveco().efforts());
    assert_eq!(provider_blocks[0], provider_blocks[1]);
}

#[test]
fn opencode_family_directories_follow_each_fork() {
    let home = tempdir().unwrap();
    let custom = home.path().join("mimo-home");
    let deveco_dir = home.path().join("deveco-config");

    let mimo_ctx = common::home_with(
        home.path(),
        &[("MIMOCODE_HOME", custom.display().to_string())],
    );
    assert_eq!(
        mimocode().config_files(&mimo_ctx),
        vec![custom.join("config").join("mimocode.json")]
    );

    let deveco_ctx = common::home_with(
        home.path(),
        &[("DEVECO_CONFIG_DIR", deveco_dir.display().to_string())],
    );
    assert_eq!(
        deveco().config_files(&deveco_ctx),
        vec![deveco_dir.join("deveco.json")]
    );

    let default_ctx = common::home(home.path());
    assert_eq!(
        mimocode().config_files(&default_ctx),
        vec![home.path().join(".config/mimocode/mimocode.json")]
    );
    assert_eq!(
        deveco().config_files(&default_ctx),
        vec![home.path().join(".config/deveco/deveco.json")]
    );
}

#[test]
fn openchamber_roundtrip_restores_preferences_settings_and_opencode() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let preferences = home.path().join(".config/openchamber/preferences.json");
    let settings = home.path().join(".config/openchamber/settings.json");
    let opencode = home.path().join(".config/opencode/opencode.json");
    let preferences_seed = concat!(
        "{\n",
        "  \"version\": 1,\n",
        "  \"theme\": \"dark\",\n",
        "  \"fields\": {\"defaultModel\": {\"value\": \"openai/gpt-5\", \"updatedAt\": 1}}\n",
        "}\n"
    );
    let settings_seed = "{\n  \"defaultModel\": \"openai/gpt-5\",\n  \"theme\": \"dark\"\n}\n";
    let opencode_seed =
        "{\n  \"theme\": \"system\",\n  \"provider\": {\"openai\": {\"name\": \"OpenAI\"}}\n}\n";
    common::write(&preferences, preferences_seed);
    common::write(&settings, settings_seed);
    common::write(&opencode, opencode_seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&OpenChamberSpec, &ctx, &wiring_ctx(&catalog, Some("high")))
        .unwrap();

    let preferences_text = common::read(&preferences);
    assert_eq!(
        json::get(&preferences_text, "fields.defaultModel.value").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&preferences_text, "fields.defaultVariant.value").as_deref(),
        Some("high")
    );
    // OpenChamber only uses smallModelOverride when the flag is false.
    assert_eq!(
        json::get(&preferences_text, "fields.smallModelUseDefault.value").as_deref(),
        Some("false")
    );
    assert_eq!(
        json::get(&preferences_text, "fields.smallModelOverride.value").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );

    let settings_text = common::read(&settings);
    assert_eq!(
        json::get(&settings_text, "defaultModel").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&settings_text, "defaultVariant").as_deref(),
        Some("high")
    );
    assert_eq!(
        json::get(&settings_text, "smallModelUseDefault").as_deref(),
        Some("false")
    );
    assert_eq!(
        json::get(&settings_text, "smallModelOverride").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );

    let opencode_text = common::read(&opencode);
    assert_eq!(
        json::get(&opencode_text, "provider.any-converter.options.baseURL").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    // The OpenCode seed has no 2.x `providers` block, and OpenCode 1 rejects
    // unknown top-level keys — openchamber must not introduce one.
    assert_eq!(
        json::get(&opencode_text, "providers.any-converter.settings.apiKey").as_deref(),
        None
    );
    assert!(opencode_text.contains("OpenAI"), "{opencode_text}");
    assert_eq!(
        OpenChamberSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&OpenChamberSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&preferences).unwrap(), preferences_seed.as_bytes());
    assert_eq!(fs::read(&settings).unwrap(), settings_seed.as_bytes());
    assert_eq!(fs::read(&opencode).unwrap(), opencode_seed.as_bytes());
    assert_eq!(
        OpenChamberSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn openchamber_removes_files_it_created() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let preferences = home.path().join(".config/openchamber/preferences.json");
    let settings = home.path().join(".config/openchamber/settings.json");
    let opencode = home.path().join(".config/opencode/opencode.json");

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&OpenChamberSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    assert_eq!(
        OpenChamberSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&OpenChamberSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert!(!preferences.exists());
    assert!(!settings.exists());
    assert!(!opencode.exists());
}

fn goose_base(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("appdata").join("Block/goose/config")
    } else {
        home.join("xdg").join("goose")
    }
}

fn goose_ctx(home: &Path) -> HomeCtx {
    common::home_with(
        home,
        &[
            ("APPDATA", home.join("appdata").display().to_string()),
            ("XDG_CONFIG_HOME", home.join("xdg").display().to_string()),
        ],
    )
}

#[test]
fn goose_legacy_flat_keys_roundtrip() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = goose_ctx(home.path());
    let catalog = common::catalog();
    let base = goose_base(home.path());
    let config = base.join("config.yaml");
    let provider = base.join("custom_providers/any-converter.json");
    let seed = "# goose config\nGOOSE_PROVIDER: openai\nGOOSE_MODEL: gpt-5\n";
    common::write(&config, seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&GooseSpec, &ctx, &wiring_ctx(&catalog, Some("high")))
        .unwrap();

    let text = common::read(&config);
    assert_eq!(
        yaml::get(&text, "GOOSE_PROVIDER").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        yaml::get(&text, "GOOSE_MODEL").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        yaml::get(&text, "GOOSE_THINKING_EFFORT").as_deref(),
        Some("high")
    );
    assert!(text.contains("# goose config"));

    let provider_value = parse(&common::read(&provider));
    assert_eq!(provider_value["name"], "any-converter");
    assert_eq!(provider_value["engine"], "openai");
    assert_eq!(provider_value["base_url"], "http://127.0.0.1:8787/v1");
    // Without these Goose fetches /v1/models and canonical-filters our list
    // away against its own registry.
    assert_eq!(provider_value["skip_canonical_filtering"], true);
    assert_eq!(provider_value["dynamic_models"], false);
    assert_eq!(
        provider_value["headers"]["Authorization"],
        format!("Bearer {}", common::AGENT_KEY)
    );
    assert_eq!(provider_value["models"][0]["name"], "claude-opus-4-5");
    assert_eq!(provider_value["models"][0]["reasoning"], true);
    assert_eq!(
        GooseSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&GooseSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&config).unwrap(), seed.as_bytes());
    assert!(!provider.exists());
    assert_eq!(
        GooseSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn goose_new_provider_shape_roundtrip() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = goose_ctx(home.path());
    let catalog = common::catalog();
    let config = goose_base(home.path()).join("config.yaml");
    let seed = concat!(
        "active_provider: openai\n",
        "providers:\n",
        "  openai:\n",
        "    enabled: true\n",
        "    model: gpt-5\n"
    );
    common::write(&config, seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&GooseSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&config);
    assert_eq!(
        yaml::get(&text, "active_provider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.enabled").as_deref(),
        Some("true")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.model").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.configured").as_deref(),
        Some("true")
    );
    assert_eq!(
        GooseSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&GooseSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&config).unwrap(), seed.as_bytes());
}

#[test]
fn goose_custom_provider_preserves_display_name_and_headers() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = goose_ctx(home.path());
    let catalog = common::catalog();
    let base = goose_base(home.path());
    let config = base.join("config.yaml");
    let provider = base.join("custom_providers/any-converter.json");
    common::write(&config, "GOOSE_PROVIDER: openai\nGOOSE_MODEL: gpt-5\n");
    common::write(
        &provider,
        "{\"display_name\": \"Mine\", \"headers\": {\"X-User\": \"1\", \"Authorization\": \"Bearer old\"}}\n",
    );

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&GooseSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&provider);
    assert!(text.contains("\"display_name\": \"Mine\""), "{text}");
    let value = parse(&text);
    assert_eq!(value["headers"]["X-User"], "1");
    assert_eq!(
        value["headers"]["Authorization"],
        format!("Bearer {}", common::AGENT_KEY)
    );

    // The descriptor is ours and is removed on disconnect; the pre-existing
    // bytes are retained as recovery data.
    let report = wiring.disconnect(&GooseSpec, &ctx).unwrap();
    assert!(!provider.exists());
    assert!(report.kept_backup.contains(&provider));
}

fn crush_paths(home: &Path) -> (PathBuf, PathBuf) {
    if cfg!(windows) {
        let path = home.join("localapp").join("crush/crush.json");
        (path.clone(), path)
    } else {
        (
            home.join("xdg").join("crush/crush.json"),
            home.join("data").join("crush/crush.json"),
        )
    }
}

fn crush_ctx(home: &Path) -> HomeCtx {
    common::home_with(
        home,
        &[
            ("LOCALAPPDATA", home.join("localapp").display().to_string()),
            ("XDG_CONFIG_HOME", home.join("xdg").display().to_string()),
            ("XDG_DATA_HOME", home.join("data").display().to_string()),
        ],
    )
}

#[test]
fn crush_roundtrip_preserves_user_model_slots() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = crush_ctx(home.path());
    let catalog = common::catalog();
    let (config, model_file) = crush_paths(home.path());
    let user_models = "{\"large\": {\"provider\": \"openai\", \"model\": \"gpt-5\", \"reasoning_effort\": \"low\"}}";
    if config == model_file {
        let seed = format!(
            "{{\n  \"providers\": {{\"openai\": {{\"type\": \"openai\"}}}},\n  \"models\": {user_models}\n}}\n"
        );
        common::write(&config, &seed);
    } else {
        common::write(
            &config,
            "{\n  \"providers\": {\"openai\": {\"type\": \"openai\"}}\n}\n",
        );
        common::write(&model_file, &format!("{{\"models\": {user_models}}}\n"));
    }
    let config_original = fs::read(&config).unwrap();
    let model_original = if config == model_file {
        config_original.clone()
    } else {
        fs::read(&model_file).unwrap()
    };

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&CrushSpec, &ctx, &wiring_ctx(&catalog, Some("high")))
        .unwrap();

    let config_text = common::read(&config);
    assert_eq!(
        json::get(&config_text, "providers.any-converter.type").as_deref(),
        Some("openai")
    );
    assert_eq!(
        json::get(&config_text, "providers.any-converter.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&config_text, "providers.any-converter.api_key").as_deref(),
        Some(common::AGENT_KEY)
    );
    let provider_models = json::get(&config_text, "providers.any-converter.models").unwrap();
    assert!(
        provider_models.contains("claude-sonnet-4-5"),
        "{provider_models}"
    );
    assert!(
        provider_models.contains("\"can_reason\":true"),
        "{provider_models}"
    );
    assert!(
        provider_models.contains("\"context_window\":200000"),
        "{provider_models}"
    );
    assert!(
        provider_models.contains("\"default_max_tokens\":16384"),
        "{provider_models}"
    );
    // Pricing is unknown in this catalog, so no cost fields are written.
    assert!(
        !provider_models.contains("cost_per_1m"),
        "{provider_models}"
    );

    let models_text = common::read(&model_file);
    assert_eq!(
        json::get(&models_text, "models.large.provider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&models_text, "models.large.model").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&models_text, "models.large.reasoning_effort").as_deref(),
        Some("high")
    );
    assert_eq!(
        json::get(&models_text, "models.small.provider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        CrushSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&CrushSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&config).unwrap(), config_original);
    if config != model_file {
        assert_eq!(fs::read(&model_file).unwrap(), model_original);
    }
    assert_eq!(
        json::get(&common::read(&model_file), "models.large.provider").as_deref(),
        Some("openai")
    );
}

#[test]
fn crush_removes_files_it_created() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = crush_ctx(home.path());
    let catalog = common::catalog();
    let (config, model_file) = crush_paths(home.path());

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&CrushSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    assert_eq!(
        CrushSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&CrushSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert!(!config.exists());
    if config != model_file {
        assert!(!model_file.exists());
    }
}

#[test]
fn openclaw_roundtrip_restores_model_and_policy() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let config = home.path().join("openclaw.json");
    let ctx = common::home_with(
        home.path(),
        &[("OPENCLAW_CONFIG_PATH", config.display().to_string())],
    );
    let catalog = common::catalog();
    let seed = concat!(
        "{\n",
        "  \"theme\": \"dark\",\n",
        "  \"agents\": {\n",
        "    \"defaults\": {\n",
        "      \"model\": \"openai/gpt-5\",\n",
        "      \"models\": {\"openai/gpt-5\": {}},\n",
        "      \"modelPolicy\": {\"allow\": [\"openai/*\"]}\n",
        "    }\n",
        "  }\n",
        "}\n"
    );
    common::write(&config, seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&OpenClawSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&config);
    assert_eq!(
        json::get(&text, "models.providers.any-converter.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "models.providers.any-converter.api").as_deref(),
        Some("openai-completions")
    );
    let models = json::get(&text, "models.providers.any-converter.models").unwrap();
    assert!(models.contains("claude-sonnet-4-5"), "{models}");
    // This catalog knows nothing about vision: every model starts text-only.
    assert!(models.contains("\"input\":[\"text\"]"), "{models}");
    assert!(!models.contains("\"image\""), "{models}");
    // `name` is the model id, `reasoning` follows the model, and a known
    // window comes from the catalog.
    let entries = parse(&models);
    let sonnet = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "claude-sonnet-4-5")
        .unwrap();
    assert_eq!(sonnet["name"], "claude-sonnet-4-5");
    assert_eq!(sonnet["reasoning"], true);
    assert_eq!(sonnet["contextWindow"], 200_000);
    let haiku = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "claude-haiku-4-5")
        .unwrap();
    assert_eq!(haiku["reasoning"], false);
    assert_eq!(
        json::get(&text, "agents.defaults.model").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );
    let defaults = parse(&json::get(&text, "agents.defaults.models").unwrap());
    assert!(defaults.get("openai/gpt-5").is_some(), "{defaults}");
    assert!(
        defaults.get("any-converter/claude-sonnet-4-5").is_some(),
        "{defaults}"
    );
    let allow = json::get(&text, "agents.defaults.modelPolicy.allow").unwrap();
    assert!(allow.contains("openai/*"), "{allow}");
    assert!(allow.contains("any-converter/*"), "{allow}");
    assert_eq!(
        OpenClawSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&OpenClawSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&config).unwrap(), seed.as_bytes());
    assert_eq!(
        OpenClawSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn openclaw_skips_policy_without_an_allow_list() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let config = home.path().join("openclaw.json");
    let ctx = common::home_with(
        home.path(),
        &[("OPENCLAW_CONFIG_PATH", config.display().to_string())],
    );
    let catalog = common::catalog();
    common::write(
        &config,
        "{\"agents\": {\"defaults\": {\"model\": \"openai/gpt-5\"}}}\n",
    );

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&OpenClawSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    assert_eq!(
        json::get(&common::read(&config), "agents.defaults.modelPolicy.allow"),
        None
    );
}

#[test]
fn snow_roundtrip_restores_all_three_files() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let root = home.path().join("snow-root");
    let ctx = common::home_with(
        home.path(),
        &[("SNOW_CONFIG_DIR", root.display().to_string())],
    );
    let catalog = common::catalog();
    let profile = root.join("profiles/any-converter.json");
    let user_profile = root.join("profiles/work.json");
    let active = root.join("active-profile.json");
    let config = root.join("config.json");
    let profile_seed = "{\"snowcfg\": {\"baseUrl\": \"https://old\", \"advancedModel\": \"old\"}, \"custom\": true}\n";
    let user_seed = concat!(
        "{\n",
        "  \"snowcfg\": {\n",
        "    \"baseUrl\": \"https://work\",\n",
        "    \"advancedModel\": \"kimi-k3\",\n",
        "    \"streamingDisplay\": false,\n",
        "    \"systemPromptId\": \"terse\",\n",
        "    \"oauth\": {\"accessToken\": \"secret\"}\n",
        "  },\n",
        "  \"companionMuted\": true\n",
        "}\n"
    );
    let active_seed = "{\n  \"activeProfile\": \"work\",\n  \"other\": 1\n}\n";
    let config_seed = "{\"snowcfg\": {\"baseUrl\": \"https://root-old\"}, \"custom\": \"root\"}\n";
    common::write(&profile, profile_seed);
    common::write(&user_profile, user_seed);
    common::write(&active, active_seed);
    common::write(&config, config_seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&SnowSpec, &ctx, &wiring_ctx(&catalog, Some("medium")))
        .unwrap();

    let text = common::read(&profile);
    assert_eq!(
        json::get(&text, "snowcfg.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "snowcfg.baseUrlMode").as_deref(),
        Some("base")
    );
    assert_eq!(
        json::get(&text, "snowcfg.apiKey").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        json::get(&text, "snowcfg.requestMethod").as_deref(),
        Some("chat")
    );
    assert_eq!(
        json::get(&text, "snowcfg.advancedModel").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&text, "snowcfg.basicModel").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&text, "snowcfg.maxContextTokens").as_deref(),
        Some("200000")
    );
    // The synced settings always come out with a value.
    assert_eq!(
        json::get(&text, "snowcfg.maxTokens").as_deref(),
        Some("64000")
    );
    assert_eq!(
        json::get(&text, "snowcfg.supportsVision").as_deref(),
        Some("false")
    );
    // The profile is seeded from the user's active profile: their other
    // settings and top-level keys survive, their sign-in does not.
    assert_eq!(
        json::get(&text, "snowcfg.streamingDisplay").as_deref(),
        Some("false")
    );
    assert_eq!(
        json::get(&text, "snowcfg.systemPromptId").as_deref(),
        Some("terse")
    );
    assert_eq!(json::get(&text, "companionMuted").as_deref(), Some("true"));
    assert_eq!(json::get(&text, "snowcfg.oauth"), None);
    assert_eq!(
        json::get(&text, "snowcfg.chatThinking.enabled").as_deref(),
        Some("true")
    );
    assert_eq!(
        json::get(&text, "snowcfg.chatThinking.reasoning_effort").as_deref(),
        Some("medium")
    );
    assert_eq!(
        json::get(&common::read(&active), "activeProfile").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&common::read(&config), "snowcfg.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        SnowSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&SnowSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&profile).unwrap(), profile_seed.as_bytes());
    assert_eq!(fs::read(&user_profile).unwrap(), user_seed.as_bytes());
    assert_eq!(fs::read(&active).unwrap(), active_seed.as_bytes());
    assert_eq!(fs::read(&config).unwrap(), config_seed.as_bytes());
    assert_eq!(
        SnowSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn snow_removes_files_it_created() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let root = home.path().join("snow-root");
    let ctx = common::home_with(
        home.path(),
        &[("SNOW_CONFIG_DIR", root.display().to_string())],
    );
    let catalog = common::catalog();

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&SnowSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    assert_eq!(
        SnowSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&SnowSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert!(!root.join("profiles/any-converter.json").exists());
    assert!(!root.join("active-profile.json").exists());
    assert!(!root.join("config.json").exists());
}

/// QwenPaw's secret folder is the working folder's sibling.
fn qwenpaw_provider_path(work: &Path) -> PathBuf {
    let mut name = work.as_os_str().to_os_string();
    name.push(".secret");
    PathBuf::from(name)
        .join("providers")
        .join("custom")
        .join("any-converter.json")
}

#[test]
fn qwenpaw_roundtrip_restores_agent_and_removes_provider() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let work = home.path().join("qwenpaw-work");
    let ctx = common::home_with(
        home.path(),
        &[("QWENPAW_WORKING_DIR", work.display().to_string())],
    );
    let catalog = common::catalog();
    let agent = work.join("workspaces/default/agent.json");
    let provider = qwenpaw_provider_path(&work);
    let seed = concat!(
        "{\n",
        "  \"active_model\": {\"provider_id\": \"other\", \"model\": \"x\"},\n",
        "  \"thinking_level\": \"low\",\n",
        "  \"theme\": \"dark\"\n",
        "}\n"
    );
    common::write(&agent, seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&QwenPawSpec, &ctx, &wiring_ctx(&catalog, Some("high")))
        .unwrap();

    let agent_text = common::read(&agent);
    assert_eq!(
        json::get(&agent_text, "active_model.provider_id").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&agent_text, "active_model.model").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&agent_text, "thinking_level").as_deref(),
        Some("high")
    );
    assert_eq!(json::get(&agent_text, "theme").as_deref(), Some("dark"));

    let provider_value = parse(&common::read(&provider));
    assert_eq!(provider_value["id"], "any-converter");
    assert_eq!(provider_value["name"], "any-converter");
    assert_eq!(provider_value["base_url"], "http://127.0.0.1:8787/v1");
    assert_eq!(provider_value["api_key"], common::AGENT_KEY);
    assert_eq!(provider_value["chat_model"], "OpenAIChatModel");
    assert_eq!(provider_value["is_custom"], true);
    assert_eq!(provider_value["enabled"], true);
    // QwenPaw's own defaults are filled in beside ours.
    assert_eq!(provider_value["support_model_discovery"], false);
    assert_eq!(provider_value["support_connection_check"], false);
    assert_eq!(provider_value["models"], serde_json::json!([]));
    assert_eq!(provider_value["generate_kwargs"], serde_json::json!({}));
    assert_eq!(provider_value["custom_headers"], serde_json::json!({}));
    assert_eq!(provider_value["snapshot_schema_version"], 2);
    let extra = &provider_value["extra_models"][0];
    assert_eq!(extra["id"], "claude-opus-4-5");
    assert_eq!(extra["source"], "user");
    assert_eq!(extra["supports_tool_calling"], true);
    assert_eq!(extra["max_input_length"], 200_000u64);
    assert_eq!(extra["thinking_param_style"], "effort");
    assert_eq!(extra["reasoning_effort_options"][3], "xhigh");
    // Non-reasoning models declare no thinking style at all.
    let haiku = provider_value["extra_models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "claude-haiku-4-5")
        .unwrap();
    assert_eq!(haiku["thinking_param_style"], Value::Null);
    assert_eq!(haiku["reasoning_effort_options"], Value::Null);
    assert_eq!(
        QwenPawSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&QwenPawSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&agent).unwrap(), seed.as_bytes());
    assert!(!provider.exists());
    assert_eq!(
        QwenPawSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn qwenpaw_keeps_provider_when_agent_still_references_it() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let work = home.path().join("qwenpaw-work");
    let ctx = common::home_with(
        home.path(),
        &[("QWENPAW_WORKING_DIR", work.display().to_string())],
    );
    let catalog = common::catalog();
    let agent = work.join("workspaces/default/agent.json");
    let provider = qwenpaw_provider_path(&work);
    common::write(
        &agent,
        "{\"active_model\": {\"provider_id\": \"any-converter\", \"model\": \"mine\"}}\n",
    );

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&QwenPawSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    let report = wiring.disconnect(&QwenPawSpec, &ctx).unwrap();

    // The provider file did not exist before connect but is intentionally kept
    // because the agent still references it, so recovery data is retained.
    assert!(report.kept_backup.contains(&provider));
    assert!(provider.exists());
    // The user's pre-existing reference to our provider came back.
    assert_eq!(
        json::get(&common::read(&agent), "active_model.provider_id").as_deref(),
        Some("any-converter")
    );
}

#[test]
fn opencode_family_native_v2_uses_v2_shape_and_drops_v1() {
    let catalog = common::catalog();
    for spec in [mimocode(), deveco()] {
        let home = tempdir().unwrap();
        let data = tempdir().unwrap();
        let ctx = common::home(home.path());
        let file = home
            .path()
            .join(".config")
            .join(spec.dir_name)
            .join(format!("{}.json", spec.dir_name));
        let seed = concat!(
            "{\n",
            "  \"theme\": \"dark\",\n",
            "  \"model\": \"openai/gpt-5\",\n",
            "  \"small_model\": \"openai/gpt-5-mini\",\n",
            "  \"providers\": {\n",
            "    \"any-converter\": {\"name\": \"Any Converter\", \"package\": \"aisdk:@ai-sdk/openai-compatible\", \"settings\": {\"baseURL\": \"https://old\", \"apiKey\": \"old\"}, \"models\": {}}\n",
            "  }\n",
            "}\n"
        );
        common::write(&file, seed);

        let wiring = Wiring::new(data.path());
        wiring
            .connect(&spec, &ctx, &wiring_ctx(&catalog, None))
            .unwrap();

        let text = common::read(&file);
        // A 2.x-native file gets the 2.x shape under `providers`, and the
        // 1.x entry is dropped as OpenCode 2 itself does on load.
        assert_eq!(
            json::get(&text, "provider.any-converter"),
            None,
            "{}",
            spec.id()
        );
        assert_eq!(
            json::get(&text, "providers.any-converter.package").as_deref(),
            Some("aisdk:@ai-sdk/openai-compatible")
        );
        assert_eq!(
            json::get(&text, "providers.any-converter.settings.baseURL").as_deref(),
            Some("http://127.0.0.1:8787/v1")
        );
        assert_eq!(
            json::get(
                &text,
                "providers.any-converter.compatibility.supportsPromptCacheKey"
            )
            .as_deref(),
            Some("true")
        );
        // The user's small model survives a connect.
        assert_eq!(
            json::get(&text, "small_model").as_deref(),
            Some("openai/gpt-5-mini")
        );
        assert_eq!(
            spec.read_state(&ctx, common::GATEWAY),
            WiredState::Connected
        );

        wiring.disconnect(&spec, &ctx).unwrap();
        let restored = common::read(&file);
        assert_eq!(json::get(&restored, "provider.any-converter"), None);
        assert_eq!(
            json::get(&restored, "providers.any-converter.settings.baseURL").as_deref(),
            Some("https://old")
        );
        assert_eq!(
            json::get(&restored, "model").as_deref(),
            Some("openai/gpt-5")
        );
    }
}

#[test]
fn opencode_family_env_overrides_require_absolute() {
    let home = tempdir().unwrap();
    let relative = common::home_with(
        home.path(),
        &[
            ("MIMOCODE_HOME", "relative-mimo".to_owned()),
            ("DEVECO_CONFIG_DIR", "relative-deveco".to_owned()),
        ],
    );
    assert_eq!(
        mimocode().config_files(&relative),
        vec![home.path().join(".config/mimocode/mimocode.json")]
    );
    assert_eq!(
        deveco().config_files(&relative),
        vec![home.path().join(".config/deveco/deveco.json")]
    );

    // A leading `~` is not an absolute path, and is not expanded first.
    let tilde = common::home_with(
        home.path(),
        &[
            ("MIMOCODE_HOME", "~/mimo".to_owned()),
            ("DEVECO_CONFIG_DIR", "~/deveco".to_owned()),
        ],
    );
    assert_eq!(
        mimocode().config_files(&tilde),
        vec![home.path().join(".config/mimocode/mimocode.json")]
    );
    assert_eq!(
        deveco().config_files(&tilde),
        vec![home.path().join(".config/deveco/deveco.json")]
    );
}

#[test]
fn opencode_2x_refreshes_only_providers_and_keeps_cache_key() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let file = home.path().join(".config/opencode/opencode.json");
    let seed = concat!(
        "{\n",
        "  \"model\": \"openai/gpt-5\",\n",
        "  \"small_model\": \"openai/gpt-5-mini\",\n",
        "  \"provider\": {\"any-converter\": {\"npm\": \"@ai-sdk/openai-compatible\", \"options\": {\"setCacheKey\": false}}},\n",
        "  \"providers\": {\n",
        "    \"any-converter\": {\"name\": \"Any Converter\", \"package\": \"aisdk:@ai-sdk/openai-compatible\", \"settings\": {\"baseURL\": \"https://old\", \"apiKey\": \"old\"}, \"compatibility\": {\"supportsPromptCacheKey\": false}, \"models\": {}}\n",
        "  }\n",
        "}\n"
    );
    common::write(&file, seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&OpenCodeSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&file);
    assert_eq!(json::get(&text, "provider.any-converter"), None);
    assert_eq!(
        json::get(&text, "providers.any-converter.settings.baseURL").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    // The user's cache-key choices are preserved.
    assert_eq!(
        json::get(
            &text,
            "providers.any-converter.compatibility.supportsPromptCacheKey"
        )
        .as_deref(),
        Some("false")
    );
    assert_eq!(
        OpenCodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    wiring.disconnect(&OpenCodeSpec, &ctx).unwrap();
    let restored = common::read(&file);
    assert_eq!(
        json::get(&restored, "model").as_deref(),
        Some("openai/gpt-5")
    );
    assert_eq!(
        json::get(&restored, "small_model").as_deref(),
        Some("openai/gpt-5-mini")
    );
    assert_eq!(
        json::get(&restored, "providers.any-converter.settings.baseURL").as_deref(),
        Some("https://old")
    );
    assert_eq!(
        json::get(&restored, "provider.any-converter.options.setCacheKey").as_deref(),
        Some("false")
    );
}

#[test]
fn opencode_restores_object_model_without_double_encoding() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let file = home.path().join(".config/opencode/opencode.json");
    let seed = concat!(
        "{\n",
        "  \"model\": {\"providerID\": \"openai\", \"model\": \"gpt-5\", \"variant\": \"high\"},\n",
        "  \"small_model\": \"openai/gpt-5-mini\"\n",
        "}\n"
    );
    common::write(&file, seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&OpenCodeSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    assert_eq!(
        json::get(&common::read(&file), "model").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );

    wiring.disconnect(&OpenCodeSpec, &ctx).unwrap();
    let restored = common::read(&file);
    assert_eq!(
        json::get(&restored, "model.providerID").as_deref(),
        Some("openai")
    );
    assert_eq!(
        json::get(&restored, "model.model").as_deref(),
        Some("gpt-5")
    );
    assert_eq!(
        json::get(&restored, "model.variant").as_deref(),
        Some("high")
    );
}

#[test]
fn openchamber_refuses_non_version_one_preferences() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let preferences = home.path().join(".config/openchamber/preferences.json");
    let settings = home.path().join(".config/openchamber/settings.json");
    let seed = "{\"version\": 2, \"fields\": {}}\n";
    common::write(&preferences, seed);

    let wiring = Wiring::new(data.path());
    let error = wiring
        .connect(&OpenChamberSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap_err();
    assert!(
        error.to_string().contains("version"),
        "unexpected error: {error}"
    );
    assert_eq!(fs::read(&preferences).unwrap(), seed.as_bytes());
    assert!(!settings.exists());
}

#[test]
fn openchamber_restores_user_provider_blocks() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let catalog = common::catalog();
    let preferences = home.path().join(".config/openchamber/preferences.json");
    let settings = home.path().join(".config/openchamber/settings.json");
    let opencode = home.path().join(".config/opencode/opencode.json");
    common::write(
        &preferences,
        "{\"version\": 1, \"fields\": {\"defaultModel\": {\"value\": \"openai/gpt-5\", \"updatedAt\": 1}}}\n",
    );
    common::write(&settings, "{\"theme\": \"dark\"}\n");
    common::write(
        &opencode,
        concat!(
            "{\n",
            "  \"provider\": {\"any-converter\": {\"npm\": \"@ai-sdk/openai-compatible\", \"name\": \"Mine\", \"options\": {\"baseURL\": \"https://user-v1\", \"apiKey\": \"mine\"}}},\n",
            "  \"providers\": {\"any-converter\": {\"name\": \"Mine\", \"package\": \"aisdk:@ai-sdk/openai-compatible\", \"settings\": {\"baseURL\": \"https://user-v2\", \"apiKey\": \"mine\"}, \"models\": {}}}\n",
            "}\n"
        ),
    );

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&OpenChamberSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    let text = common::read(&opencode);
    assert_eq!(
        json::get(&text, "provider.any-converter.options.baseURL").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.settings.baseURL").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );

    wiring.disconnect(&OpenChamberSpec, &ctx).unwrap();
    let restored = common::read(&opencode);
    assert_eq!(
        json::get(&restored, "provider.any-converter.options.baseURL").as_deref(),
        Some("https://user-v1")
    );
    assert_eq!(
        json::get(&restored, "providers.any-converter.settings.baseURL").as_deref(),
        Some("https://user-v2")
    );
}

#[test]
fn openchamber_read_state_uses_preferences_when_settings_lacks_model() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let preferences = home.path().join(".config/openchamber/preferences.json");
    let settings = home.path().join(".config/openchamber/settings.json");
    let opencode = home.path().join(".config/opencode/opencode.json");
    common::write(
        &preferences,
        "{\"version\": 1, \"fields\": {\"defaultModel\": {\"value\": \"any-converter/claude-sonnet-4-5\", \"updatedAt\": 1}}}\n",
    );
    common::write(&settings, "{\"theme\": \"dark\"}\n");
    common::write(
        &opencode,
        concat!(
            "{\n",
            "  \"provider\": {\"any-converter\": {\"npm\": \"@ai-sdk/openai-compatible\", \"options\": {\"baseURL\": \"http://127.0.0.1:8787/v1\", \"apiKey\": \"acw_test_agent_key\"}}}\n",
            "}\n"
        ),
    );
    assert_eq!(
        OpenChamberSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
}

#[test]
fn goose_merges_user_provider_keys() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = goose_ctx(home.path());
    let catalog = common::catalog();
    let base = goose_base(home.path());
    let config = base.join("config.yaml");
    let provider = base.join("custom_providers/any-converter.json");
    common::write(&config, "GOOSE_PROVIDER: openai\nGOOSE_MODEL: gpt-5\n");
    common::write(
        &provider,
        concat!(
            "{\n",
            "  \"name\": \"Mine\",\n",
            "  \"display_name\": \"Mine\",\n",
            "  \"timeout_seconds\": 42,\n",
            "  \"toolshim\": true,\n",
            "  \"supports_streaming\": true,\n",
            "  \"preserves_thinking\": true,\n",
            "  \"headers\": {\"X-User\": \"1\"}\n",
            "}\n"
        ),
    );

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&GooseSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let value = parse(&common::read(&provider));
    assert_eq!(value["name"], "Mine");
    assert_eq!(value["display_name"], "Mine");
    assert_eq!(value["timeout_seconds"], 42);
    assert_eq!(value["toolshim"], true);
    assert_eq!(value["supports_streaming"], true);
    assert_eq!(value["preserves_thinking"], true);
    assert_eq!(value["engine"], "openai");
    assert_eq!(value["skip_canonical_filtering"], true);
    assert_eq!(value["dynamic_models"], false);
    assert_eq!(value["headers"]["X-User"], "1");
    assert_eq!(
        value["headers"]["Authorization"],
        format!("Bearer {}", common::AGENT_KEY)
    );
}

#[test]
fn crush_reconnect_keeps_effort() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = crush_ctx(home.path());
    let catalog = common::catalog();
    let (_, model_file) = crush_paths(home.path());
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&CrushSpec, &ctx, &wiring_ctx(&catalog, Some("high")))
        .unwrap();
    wiring
        .connect(&CrushSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();
    assert_eq!(
        json::get(&common::read(&model_file), "models.large.reasoning_effort").as_deref(),
        Some("high")
    );

    // An explicit empty effort clears it.
    wiring
        .connect(&CrushSpec, &ctx, &wiring_ctx(&catalog, Some("")))
        .unwrap();
    assert_eq!(
        json::get(&common::read(&model_file), "models.large.reasoning_effort"),
        None
    );
}

#[test]
fn crush_defaults_unknown_window() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = crush_ctx(home.path());
    let catalog = ModelCatalog::new(vec![CatalogModel::new(
        common::DEFAULT_MODEL,
        "Mystery",
        false,
        None,
    )]);
    let (config, _) = crush_paths(home.path());

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&CrushSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&config);
    let models = json::get(&text, "providers.any-converter.models").unwrap();
    assert!(models.contains("\"context_window\":200000"), "{models}");
    assert!(models.contains("\"default_max_tokens\":16384"), "{models}");
    assert!(models.contains("\"can_reason\":false"), "{models}");
}

#[test]
fn openclaw_keeps_object_model_and_user_entries() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let config = home.path().join("openclaw.json");
    let ctx = common::home_with(
        home.path(),
        &[("OPENCLAW_CONFIG_PATH", config.display().to_string())],
    );
    let catalog = common::catalog();
    let seed = concat!(
        "{\n",
        "  \"theme\": \"dark\",\n",
        "  \"agents\": {\n",
        "    \"defaults\": {\n",
        "      \"model\": {\"primary\": \"openai/gpt-5\", \"fallbacks\": [\"openai/gpt-5-mini\"]},\n",
        "      \"models\": {\"openai/gpt-5\": {}, \"any-converter/user-entry\": {\"alias\": \"mine\"}},\n",
        "      \"modelPolicy\": {\"allow\": [\"openai/*\"]}\n",
        "    }\n",
        "  }\n",
        "}\n"
    );
    common::write(&config, seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&OpenClawSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&config);
    // The fallbacks survive: only `.primary` moves.
    assert_eq!(
        json::get(&text, "agents.defaults.model.primary").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&text, "agents.defaults.model.fallbacks.0").as_deref(),
        Some("openai/gpt-5-mini")
    );
    assert_eq!(
        json::get(
            &text,
            "agents.defaults.models.any-converter/user-entry.alias"
        )
        .as_deref(),
        Some("mine")
    );
    assert_eq!(
        OpenClawSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    wiring.disconnect(&OpenClawSpec, &ctx).unwrap();
    let restored = common::read(&config);
    assert_eq!(
        json::get(&restored, "agents.defaults.model.primary").as_deref(),
        Some("openai/gpt-5")
    );
    assert_eq!(
        json::get(&restored, "agents.defaults.model.fallbacks.0").as_deref(),
        Some("openai/gpt-5-mini")
    );
    // The user's own any-converter entry (with settings) survives, while the
    // empty entry we added goes.
    assert_eq!(
        json::get(
            &restored,
            "agents.defaults.models.any-converter/user-entry.alias"
        )
        .as_deref(),
        Some("mine")
    );
    assert_eq!(
        json::get(
            &restored,
            "agents.defaults.models.any-converter/claude-sonnet-4-5"
        ),
        None
    );
    assert_eq!(
        json::get(&restored, "agents.defaults.modelPolicy.allow").as_deref(),
        Some("[\"openai/*\"]")
    );
}

#[test]
fn openclaw_home_and_profile_resolution() {
    let home = tempdir().unwrap();
    let custom = home.path().join("claw-home");
    let ctx = common::home_with(
        home.path(),
        &[
            ("OPENCLAW_HOME", custom.display().to_string()),
            ("OPENCLAW_PROFILE", "work".to_owned()),
        ],
    );
    assert_eq!(
        OpenClawSpec.config_files(&ctx),
        vec![custom.join(".openclaw-work/openclaw.json")]
    );

    let tilde = common::home_with(home.path(), &[("OPENCLAW_HOME", "~/claw".to_owned())]);
    assert_eq!(
        OpenClawSpec.config_files(&tilde),
        vec![home.path().join("claw/.openclaw/openclaw.json")]
    );

    let bad_profile = common::home_with(
        home.path(),
        &[("OPENCLAW_PROFILE", "not a profile".to_owned())],
    );
    assert_eq!(
        OpenClawSpec.config_files(&bad_profile),
        vec![home.path().join(".openclaw/openclaw.json")]
    );
}

#[test]
fn snow_seeds_from_legacy_active_profile() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let root = home.path().join("snow-root");
    let ctx = common::home_with(
        home.path(),
        &[("SNOW_CONFIG_DIR", root.display().to_string())],
    );
    let catalog = common::catalog();
    let user_profile = root.join("profiles/work.json");
    let legacy = root.join("active-profile.txt");
    let user_seed = "{\"snowcfg\": {\"advancedModel\": \"kimi-k3\", \"systemPromptId\": \"terse\"}, \"companionMuted\": true}\n";
    common::write(&user_profile, user_seed);
    common::write(&legacy, "work\n");

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&SnowSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let ours = common::read(&root.join("profiles/any-converter.json"));
    // Seeded from the legacy-named active profile.
    assert_eq!(
        json::get(&ours, "snowcfg.systemPromptId").as_deref(),
        Some("terse")
    );
    assert_eq!(json::get(&ours, "companionMuted").as_deref(), Some("true"));
    assert_eq!(
        json::get(
            &common::read(&root.join("active-profile.json")),
            "activeProfile"
        )
        .as_deref(),
        Some("any-converter")
    );

    wiring.disconnect(&SnowSpec, &ctx).unwrap();
    assert!(!root.join("profiles/any-converter.json").exists());
    assert!(!root.join("active-profile.json").exists());
    assert_eq!(fs::read(&legacy).unwrap(), b"work\n");
    assert_eq!(fs::read(&user_profile).unwrap(), user_seed.as_bytes());

    // A relative SNOW_CONFIG_DIR is ignored, as Snow ignores it.
    let relative = common::home_with(
        home.path(),
        &[("SNOW_CONFIG_DIR", "relative-snow".to_owned())],
    );
    assert_eq!(
        SnowSpec.config_files(&relative),
        vec![
            home.path().join(".snow/profiles/any-converter.json"),
            home.path().join(".snow/active-profile.json"),
            home.path().join(".snow/config.json"),
        ]
    );
}

#[test]
fn hermes_merges_existing_provider_entry() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let root = home.path().join("hermes");
    let ctx = common::home_with(home.path(), &[("HERMES_HOME", root.display().to_string())]);
    let catalog = common::catalog();
    let config = root.join("config.yaml");
    let seed = concat!(
        "model:\n",
        "  provider: openai\n",
        "  default: gpt-5\n",
        "  model: fallback-model\n",
        "providers:\n",
        "  any-converter:\n",
        "    name: My Hermes\n",
        "    default_model: old-model\n",
        "    key_env: MY_KEY\n",
        "    request_timeout_seconds: 30\n",
        "    extra_body:\n",
        "      temperature: 0.2\n",
        "    context_length: 1234\n",
        "    enabled: true\n",
        "    ssl_verify: false\n",
        "    extra_headers:\n",
        "      X-User: \"1\"\n",
        "    api_mode: anthropic_messages\n"
    );
    common::write(&config, seed);

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&HermesSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let text = common::read(&config);
    assert_eq!(
        yaml::get(&text, "providers.any-converter.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.api_key").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.api_mode").as_deref(),
        Some("chat_completions")
    );
    // Everything else the user or Hermes wrote stays.
    assert_eq!(
        yaml::get(&text, "providers.any-converter.name").as_deref(),
        Some("My Hermes")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.default_model").as_deref(),
        Some("old-model")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.key_env").as_deref(),
        Some("MY_KEY")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.request_timeout_seconds").as_deref(),
        Some("30")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.extra_body.temperature").as_deref(),
        Some("0.2")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.context_length").as_deref(),
        Some("1234")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.enabled").as_deref(),
        Some("true")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.ssl_verify").as_deref(),
        Some("false")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.extra_headers.X-User").as_deref(),
        Some("1")
    );
    assert_eq!(
        yaml::get(&text, "providers.any-converter.extra_headers.User-Agent").as_deref(),
        Some("hermes-agent")
    );
    let models = yaml::get(&text, "providers.any-converter.models").unwrap();
    assert!(models.contains("claude-sonnet-4-5"), "{models}");
    // `model.model` is Hermes' fallback and is never removed on connect.
    assert_eq!(
        yaml::get(&text, "model.model").as_deref(),
        Some("fallback-model")
    );
    // No Anthropic companion entry (see the spec's documented deviation).
    assert_eq!(yaml::get(&text, "providers.any-converter-anthropic"), None);

    wiring.disconnect(&HermesSpec, &ctx).unwrap();
    let restored = common::read(&config);
    assert_eq!(
        yaml::get(&restored, "model.provider").as_deref(),
        Some("openai")
    );
    assert_eq!(
        yaml::get(&restored, "model.default").as_deref(),
        Some("gpt-5")
    );
    assert_eq!(
        yaml::get(&restored, "model.model").as_deref(),
        Some("fallback-model")
    );
}

#[test]
fn qwenpaw_uses_legacy_copaw_and_dotenv_secret() {
    let home = tempdir().unwrap();
    let legacy = home.path().join(".copaw");
    fs::create_dir_all(&legacy).unwrap();
    let ctx = common::home(home.path());
    assert_eq!(
        QwenPawSpec.config_files(&ctx),
        vec![
            legacy.join("workspaces/default/agent.json"),
            qwenpaw_provider_path(&legacy),
        ]
    );

    let home2 = tempdir().unwrap();
    let data = tempdir().unwrap();
    let work = home2.path().join("qwenpaw-work");
    let secret = home2.path().join("custom-secret");
    let ctx2 = common::home_with(
        home2.path(),
        &[("QWENPAW_WORKING_DIR", work.display().to_string())],
    );
    common::write(
        &work.join(".env"),
        &format!(
            "# QwenPaw's settings\nexport QWENPAW_SECRET_DIR=\"{}\"\n",
            secret.display()
        ),
    );
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());
    wiring
        .connect(&QwenPawSpec, &ctx2, &wiring_ctx(&catalog, None))
        .unwrap();

    assert!(work.join("workspaces/default/agent.json").exists());
    assert!(secret.join("providers/custom/any-converter.json").exists());
}

#[test]
fn qwenpaw_keeps_provider_for_subagent_and_fallback_models() {
    let catalog = common::catalog();
    for (name, seed) in [
        (
            "subagent",
            "{\"active_model\": {\"provider_id\": \"other\", \"model\": \"x\"}, \"subagent_model\": {\"provider_id\": \"any-converter\", \"model\": \"mine\"}}\n",
        ),
        (
            "fallback",
            "{\"active_model\": {\"provider_id\": \"other\", \"model\": \"x\"}, \"fallback_models\": [{\"provider_id\": \"any-converter\", \"model\": \"mine\"}]}\n",
        ),
    ] {
        let home = tempdir().unwrap();
        let data = tempdir().unwrap();
        let work = home.path().join("qwenpaw-work");
        let ctx = common::home_with(
            home.path(),
            &[("QWENPAW_WORKING_DIR", work.display().to_string())],
        );
        let agent = work.join("workspaces/default/agent.json");
        let provider = qwenpaw_provider_path(&work);
        common::write(&agent, seed);

        let wiring = Wiring::new(data.path());
        wiring
            .connect(&QwenPawSpec, &ctx, &wiring_ctx(&catalog, None))
            .unwrap();
        let report = wiring.disconnect(&QwenPawSpec, &ctx).unwrap();

        assert!(provider.exists(), "{name}");
        assert!(report.kept_backup.contains(&provider), "{name}");
        assert_eq!(
            json::get(&common::read(&agent), "active_model.provider_id").as_deref(),
            Some("other"),
            "{name}"
        );
    }
}

#[test]
fn qwenpaw_merges_existing_provider_defaults() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let work = home.path().join("qwenpaw-work");
    let ctx = common::home_with(
        home.path(),
        &[("QWENPAW_WORKING_DIR", work.display().to_string())],
    );
    let catalog = common::catalog();
    let provider = qwenpaw_provider_path(&work);
    common::write(
        &provider,
        concat!(
            "{\n",
            "  \"enabled\": false,\n",
            "  \"custom_headers\": {\"X-User\": \"1\"},\n",
            "  \"timeout\": 30,\n",
            "  \"extra_models\": [{\"id\": \"claude-opus-4-5\", \"name\": \"Opus\", \"max_output_length\": 4096}]\n",
            "}\n"
        ),
    );

    let wiring = Wiring::new(data.path());
    wiring
        .connect(&QwenPawSpec, &ctx, &wiring_ctx(&catalog, None))
        .unwrap();

    let value = parse(&common::read(&provider));
    // Unknown provider keys and headers survive; our owned keys win.
    assert_eq!(value["timeout"], 30);
    assert_eq!(value["custom_headers"]["X-User"], "1");
    // A user's `enabled` choice is not one of ours to force.
    assert_eq!(value["enabled"], false);
    assert_eq!(value["base_url"], "http://127.0.0.1:8787/v1");
    assert_eq!(value["id"], "any-converter");
    // The existing model entry's user-set output limit survives.
    let opus = value["extra_models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "claude-opus-4-5")
        .unwrap();
    assert_eq!(opus["max_output_length"], 4096);
    assert_eq!(opus["thinking_param_style"], "effort");
}
