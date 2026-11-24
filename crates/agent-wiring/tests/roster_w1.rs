#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use agent_wiring::edit::{json, toml, yaml};
use agent_wiring::spec::liveagent::LiveagentSpec;
use agent_wiring::spec::minimax_code::MinimaxCodeSpec;
use agent_wiring::spec::morph::MorphSpec;
use agent_wiring::spec::mothx::MothxSpec;
use agent_wiring::spec::omp::OmpSpec;
use agent_wiring::spec::stepcode::StepcodeSpec;
use agent_wiring::spec::{AgentSpec, WiredState, WiringCtx};
use agent_wiring::sync::catalog::{CatalogModel, ModelCatalog};
use agent_wiring::wiring::Wiring;
use tempfile::tempdir;

/// The model every test selects; it is also the catalog's first entry.
const MODEL: &str = "deepseek/gpt-5.1";

/// Two models are enough to prove the catalog is rendered, not hardcoded.
fn catalog() -> ModelCatalog {
    ModelCatalog::new(vec![
        CatalogModel::new(MODEL, "DeepSeek GPT-5.1", true, Some(128_000)),
        CatalogModel::new("qwen/qwen3-max", "Qwen3 Max", false, Some(256_000)),
    ])
}

fn wiring_ctx<'a>(catalog: &'a ModelCatalog) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: MODEL,
        effort: Some("high"),
        roles: &common::NO_ROLES,
    }
}

fn env_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

#[test]
fn stepcode_connect_writes_and_disconnect_restores() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let config = home.path().join(".stepcode/config.toml");
    let models = home.path().join(".stepcode/models.json");
    let original_config = "theme = \"dark\"\nenabledModels = [\"openai/gpt\"]\n";
    let original_models = "{\n  \"providers\": {\"openai\": {\"name\": \"OpenAI\"}}\n}\n";
    common::write(&config, original_config);
    common::write(&models, original_models);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&StepcodeSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let config_text = common::read(&config);
    assert_eq!(
        toml::get(&config_text, "defaultProvider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        toml::get(&config_text, "defaultModel").as_deref(),
        Some(MODEL)
    );
    let enabled = toml::get(&config_text, "enabledModels").unwrap();
    assert!(enabled.contains("openai/gpt"), "{enabled}");
    // Entries use the `<provider>/<model>` form, not a bare id.
    assert!(
        enabled.contains("\"any-converter/deepseek/gpt-5.1\""),
        "{enabled}"
    );

    let models_text = common::read(&models);
    assert_eq!(
        json::get(&models_text, "providers.any-converter.name").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&models_text, "providers.any-converter.api").as_deref(),
        Some("openai-completions")
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
        json::get(&models_text, "providers.any-converter.models.0.id").as_deref(),
        Some(MODEL)
    );
    // The window key is `contextWindow`, not `context`.
    assert_eq!(
        json::get(
            &models_text,
            "providers.any-converter.models.0.contextWindow"
        )
        .as_deref(),
        Some("128000")
    );
    assert_eq!(
        json::get(&models_text, "providers.any-converter.models.0.context"),
        None
    );
    assert_eq!(
        StepcodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&StepcodeSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&config).unwrap(), original_config.as_bytes());
    assert_eq!(fs::read(&models).unwrap(), original_models.as_bytes());
    assert_eq!(
        StepcodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn mothx_connect_writes_and_disconnect_restores() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let dir = home.path().join("mothx");
    let path = dir.join("settings.json");
    let ctx = common::home_with(home.path(), &[("MOTHX_DIR", env_path(&dir))]);
    let original = "{\n  \"theme\": \"dark\",\n  \"providers\": {\"other\": {\"baseUrl\": \"https://example.test/v1\"}}\n}\n";
    common::write(&path, original);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&MothxSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "defaultProvider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(json::get(&text, "defaultModel").as_deref(), Some(MODEL));
    assert_eq!(
        json::get(&text, "defaultThinkingLevel").as_deref(),
        Some("high")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.api").as_deref(),
        Some("openai-chat")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.thinkingFormat").as_deref(),
        Some("openai")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.apiKey").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.models.0.id").as_deref(),
        Some(MODEL)
    );
    // `reasoning` mirrors the catalog flag (this model reasons).
    assert_eq!(
        json::get(&text, "providers.any-converter.models.0.reasoning").as_deref(),
        Some("true")
    );
    // The second model does not reason, so it must not claim to.
    assert_eq!(
        json::get(&text, "providers.any-converter.models.1.reasoning").as_deref(),
        Some("false")
    );
    // Catalog limitation: we cannot know image support, so text only.
    assert_eq!(
        json::get(&text, "providers.any-converter.models.0.input").as_deref(),
        Some("[\"text\"]")
    );
    assert_eq!(
        MothxSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
    assert_eq!(MothxSpec.efforts(), &["low", "medium", "high", "xhigh"]);

    let report = wiring.disconnect(&MothxSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn omp_connect_writes_and_disconnect_restores() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let agent = home.path().join("omp-agent");
    let config = agent.join("config.yml");
    let models = agent.join("models.yml");
    let ctx = common::home_with(home.path(), &[("PI_CODING_AGENT_DIR", env_path(&agent))]);
    let original_config = concat!(
        "modelRoles:\n",
        "  user_model: \"openai/gpt-5\"\n",
        "enabledModels:\n",
        "  - \"user/one\"\n",
    );
    let original_models = "providers:\n  openai:\n    baseUrl: \"https://api.openai.com/v1\"\n";
    common::write(&config, original_config);
    common::write(&models, original_models);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&OmpSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let config_text = common::read(&config);
    assert_eq!(
        yaml::get(&config_text, "modelRoles.default").as_deref(),
        Some("any-converter/deepseek/gpt-5.1")
    );
    // The user's own role is stashed, not replaced, and comes back intact.
    assert_eq!(
        yaml::get(&config_text, "modelRoles.user_model").as_deref(),
        Some("openai/gpt-5")
    );
    assert_eq!(
        yaml::get(&config_text, "defaultThinkingLevel").as_deref(),
        Some("high")
    );

    let models_text = common::read(&models);
    assert_eq!(
        yaml::get(&models_text, "providers.any-converter.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        yaml::get(&models_text, "providers.any-converter.api").as_deref(),
        Some("openai-completions")
    );
    assert_eq!(
        yaml::get(&models_text, "providers.any-converter.headers.User-Agent").as_deref(),
        Some("omp")
    );
    let rendered = yaml::get(&models_text, "providers.any-converter.models").unwrap();
    assert!(rendered.contains(MODEL), "{rendered}");
    // thinking.mode is the literal "effort", not the selected level.
    assert!(rendered.contains("\"mode\":\"effort\""), "{rendered}");
    assert!(rendered.contains("\"high\""), "{rendered}");
    // `max` makes older omp reject the whole models.yml.
    assert!(!rendered.contains("\"max\""), "{rendered}");
    // A non-reasoning model gets no thinking block.
    assert!(
        rendered.contains("\"id\":\"qwen/qwen3-max\",\"name\":\"Qwen3 Max\",\"reasoning\":false"),
        "{rendered}"
    );

    // The default role's model joins an existing enabledModels allow-list.
    assert_eq!(
        yaml::get(&config_text, "enabledModels").as_deref(),
        Some("[\"user/one\",\"any-converter/**\"]")
    );
    assert_eq!(
        OmpSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
    assert_eq!(
        OmpSpec.efforts(),
        &["minimal", "low", "medium", "high", "xhigh", "max"]
    );

    let report = wiring.disconnect(&OmpSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&config).unwrap(), original_config.as_bytes());
    assert_eq!(fs::read(&models).unwrap(), original_models.as_bytes());
}

#[test]
fn morph_connect_writes_and_disconnect_restores() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let path = home.path().join("morph-config.yaml");
    let ctx = common::home_with(home.path(), &[("MISTER_MORPH_CONFIG", env_path(&path))]);
    let original =
        "llm:\n  inference_provider: \"openai_compatible\"\n  headers:\n    X-Custom: \"one\"\n";
    common::write(&path, original);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&MorphSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&path);
    assert_eq!(
        yaml::get(&text, "llm.inference_provider").as_deref(),
        Some("openai_response_compatible")
    );
    assert_eq!(
        yaml::get(&text, "llm.endpoint").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        yaml::get(&text, "llm.api_key").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(yaml::get(&text, "llm.model").as_deref(), Some(MODEL));
    assert_eq!(
        yaml::get(&text, "llm.headers.User-Agent").as_deref(),
        Some("mistermorph")
    );
    assert_eq!(
        yaml::get(&text, "llm.context_window_tokens").as_deref(),
        Some("128000")
    );
    assert_eq!(
        yaml::get(&text, "llm.reasoning_effort").as_deref(),
        Some("high")
    );
    assert_eq!(
        MorphSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
    assert_eq!(
        MorphSpec.efforts(),
        &["minimal", "low", "medium", "high", "xhigh", "max"]
    );

    let report = wiring.disconnect(&MorphSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn minimax_connect_writes_and_disconnect_restores() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let dir = home.path().join("minimax-data");
    let path = dir.join("config.yaml");
    let ctx = common::home_with(home.path(), &[("MINIMAX_DATA_DIR", env_path(&dir))]);
    let original = concat!(
        "defaultModel: \"custom_provider:other/model\"\n",
        "defaultModelVariant: \"thinking\"\n",
        "defaultLightModel: \"user/light\"\n",
        "memory:\n",
        "  captureModel: \"user/memory\"\n",
    );
    common::write(&path, original);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&MinimaxCodeSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&path);
    // Flat ids: every slash in a model id becomes `~`.
    assert_eq!(
        yaml::get(&text, "defaultModel").as_deref(),
        Some("custom_provider:any-converter/deepseek~gpt-5.1")
    );
    // The variant belongs to the previous model and is removed.
    assert_eq!(yaml::get(&text, "defaultModelVariant"), None);
    // User picks are recorded and restored rather than clobbered; absent
    // selectors are never created.
    assert_eq!(
        yaml::get(&text, "defaultLightModel").as_deref(),
        Some("user/light")
    );
    assert_eq!(
        yaml::get(&text, "memory.captureModel").as_deref(),
        Some("user/memory")
    );
    assert_eq!(yaml::get(&text, "memory.consolidationModel"), None);
    assert_eq!(
        yaml::get(&text, "custom_provider.any-converter.name").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        yaml::get(&text, "custom_provider.any-converter.kind").as_deref(),
        Some("custom")
    );
    assert_eq!(
        yaml::get(&text, "custom_provider.any-converter.api").as_deref(),
        Some("anthropic-messages")
    );
    assert_eq!(
        yaml::get(&text, "custom_provider.any-converter.options.baseURL").as_deref(),
        Some(common::GATEWAY)
    );
    assert_eq!(
        yaml::get(&text, "custom_provider.any-converter.options.apiKey").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        yaml::get(
            &text,
            "custom_provider.any-converter.options.headers.User-Agent"
        )
        .as_deref(),
        Some("minimax-code")
    );
    let rendered = yaml::get(&text, "custom_provider.any-converter.models").unwrap();
    // The models map is keyed by the flat id.
    assert!(rendered.contains("deepseek~gpt-5.1"), "{rendered}");
    assert!(rendered.contains("\"context\":128000"), "{rendered}");
    assert!(
        rendered.contains("\"defaultEffort\":\"high\""),
        "{rendered}"
    );
    // A reasoning model gets the effort ladder.
    assert_eq!(
        yaml::get(
            &text,
            "custom_provider.any-converter.models.deepseek~gpt-5\\.1.reasoning"
        )
        .as_deref(),
        Some("true")
    );
    // A non-reasoning model gets neither `reasoning: true` nor `thinking`.
    assert_eq!(
        yaml::get(
            &text,
            "custom_provider.any-converter.models.qwen~qwen3-max.reasoning"
        )
        .as_deref(),
        Some("false")
    );
    assert_eq!(
        yaml::get(
            &text,
            "custom_provider.any-converter.models.qwen~qwen3-max.thinking"
        ),
        None
    );
    assert_eq!(
        MinimaxCodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
    assert_eq!(MinimaxCodeSpec.efforts(), &["low", "medium", "high"]);

    let report = wiring.disconnect(&MinimaxCodeSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn liveagent_connect_writes_three_providers_and_restores() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let dir = home.path().join("live");
    let path = dir.join("config.json");
    let ctx = common::home_with(home.path(), &[("LIVEAGENT_HOME", env_path(&dir))]);
    let original = "{\n  \"defaultProvider\": \"demo\",\n  \"defaultModel\": \"model1\",\n  \"providers\": {}\n}\n";
    common::write(&path, original);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&LiveagentSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "providers.any-converter.type").as_deref(),
        Some("codex")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.api").as_deref(),
        Some("openai-completions")
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
        json::get(&text, "providers.any-converter.models.0.id").as_deref(),
        Some(MODEL)
    );
    // Catalog limitation: no image data, so text-only input.
    assert_eq!(
        json::get(&text, "providers.any-converter.models.0.input").as_deref(),
        Some("[\"text\"]")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter-responses.api").as_deref(),
        Some("openai-responses")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter-responses.requestFormat").as_deref(),
        Some("openai-responses")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter-anthropic.type").as_deref(),
        Some("claude_code")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter-anthropic.api").as_deref(),
        Some("anthropic-messages")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter-anthropic.baseUrl").as_deref(),
        Some(common::GATEWAY)
    );
    assert_eq!(json::get(&text, "defaultModel").as_deref(), Some(MODEL));
    assert_eq!(
        json::get(&text, "defaultProvider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        LiveagentSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
    assert!(LiveagentSpec.efforts().is_empty());

    let report = wiring.disconnect(&LiveagentSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn liveagent_keeps_user_defaults() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let dir = home.path().join("live");
    let path = dir.join("config.json");
    let ctx = common::home_with(home.path(), &[("LIVEAGENT_HOME", env_path(&dir))]);
    let original = "{\n  \"defaultProvider\": \"anthropic\",\n  \"defaultModel\": \"user/model\",\n  \"providers\": {}\n}\n";
    common::write(&path, original);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&LiveagentSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "defaultProvider").as_deref(),
        Some("anthropic")
    );
    assert_eq!(
        json::get(&text, "defaultModel").as_deref(),
        Some("user/model")
    );
    // The providers are still registered for the user to pick.
    assert_eq!(
        json::get(&text, "providers.any-converter.type").as_deref(),
        Some("codex")
    );

    let report = wiring.disconnect(&LiveagentSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn omp_restores_list_valued_default_role() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let agent = home.path().join("omp-agent");
    let config = agent.join("config.yml");
    let models = agent.join("models.yml");
    let ctx = common::home_with(home.path(), &[("PI_CODING_AGENT_DIR", env_path(&agent))]);
    // omp falls back through a YAML list; the stash keeps it as JSON and the
    // restore must write a sequence, not an escaped string.
    let original_config = concat!(
        "modelRoles:\n",
        "  default:\n",
        "    - \"openai/gpt-5\"\n",
        "    - \"openai/gpt-5-mini\"\n",
    );
    let original_models = "providers:\n  openai:\n    baseUrl: \"https://api.openai.com/v1\"\n";
    common::write(&config, original_config);
    common::write(&models, original_models);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&OmpSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert_eq!(
        yaml::get(&common::read(&config), "modelRoles.default").as_deref(),
        Some("any-converter/deepseek/gpt-5.1")
    );

    let report = wiring.disconnect(&OmpSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    // The restored default is a YAML sequence again.
    assert_eq!(
        yaml::get(&common::read(&config), "modelRoles.default").as_deref(),
        Some("[\"openai/gpt-5\",\"openai/gpt-5-mini\"]")
    );
    assert_eq!(fs::read(&config).unwrap(), original_config.as_bytes());
    assert_eq!(fs::read(&models).unwrap(), original_models.as_bytes());
}

#[test]
fn morph_deletes_stale_context_window() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let path = home.path().join("morph-config.yaml");
    let ctx = common::home_with(home.path(), &[("MISTER_MORPH_CONFIG", env_path(&path))]);
    let original = concat!(
        "llm:\n",
        "  inference_provider: \"openai_compatible\"\n",
        "  context_window_tokens: 4242\n",
    );
    common::write(&path, original);
    // The selected model has no window in this catalog.
    let catalog = ModelCatalog::new(vec![CatalogModel::new(
        MODEL,
        "DeepSeek GPT-5.1",
        true,
        None,
    )]);
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&MorphSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert_eq!(
        yaml::get(&common::read(&path), "llm.context_window_tokens"),
        None
    );

    let report = wiring.disconnect(&MorphSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn liveagent_does_not_write_half_a_default_pair() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let dir = home.path().join("live");
    let path = dir.join("config.json");
    let ctx = common::home_with(home.path(), &[("LIVEAGENT_HOME", env_path(&dir))]);
    // The provider is still the demo one but the model is the user's: neither
    // pick may be rewritten, which would split the pair.
    let original = "{\n  \"defaultProvider\": \"demo\",\n  \"defaultModel\": \"user/model\",\n  \"providers\": {}\n}\n";
    common::write(&path, original);
    let catalog = catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&LiveagentSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let text = common::read(&path);
    assert_eq!(json::get(&text, "defaultProvider").as_deref(), Some("demo"));
    assert_eq!(
        json::get(&text, "defaultModel").as_deref(),
        Some("user/model")
    );
    // The providers are still registered for the user to pick.
    assert_eq!(
        json::get(&text, "providers.any-converter.type").as_deref(),
        Some("codex")
    );

    let report = wiring.disconnect(&LiveagentSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn stepcode_has_no_effort_setting() {
    assert!(StepcodeSpec.efforts().is_empty());
}
