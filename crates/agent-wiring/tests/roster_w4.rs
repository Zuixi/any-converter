#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use agent_wiring::edit::{json, toml};
use agent_wiring::spec::atomcode::AtomCodeSpec;
use agent_wiring::spec::claude_science::ClaudeScienceSpec;
use agent_wiring::spec::codebuddy::CodeBuddySpec;
use agent_wiring::spec::craft_agents::CraftAgentsSpec;
use agent_wiring::spec::grok::GrokSpec;
use agent_wiring::spec::kimi::KimiSpec;
use agent_wiring::spec::openbot::OpenBotSpec;
use agent_wiring::spec::openmausbot::OpenMausBotSpec;
use agent_wiring::spec::openpencil::OpenPencilSpec;
use agent_wiring::spec::pencil::PencilSpec;
use agent_wiring::spec::tuanjie_cowork::TuanjieCoworkSpec;
use agent_wiring::spec::workbuddy::{workbuddy, workbuddy_ai};
use agent_wiring::spec::{AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx};
use agent_wiring::wiring::Wiring;
use agent_wiring::{CatalogModel, ModelCatalog, WiringError};
use tempfile::tempdir;

fn wiring_ctx<'a>(catalog: &'a ModelCatalog) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: common::DEFAULT_MODEL,
        effort: None,
        roles: &common::NO_ROLES,
    }
}

fn wiring_ctx_effort<'a>(catalog: &'a ModelCatalog, effort: Option<&'a str>) -> WiringCtx<'a> {
    WiringCtx {
        effort,
        ..wiring_ctx(catalog)
    }
}

fn wiring_ctx_default<'a>(catalog: &'a ModelCatalog, model: &'a str) -> WiringCtx<'a> {
    WiringCtx {
        default_model: model,
        ..wiring_ctx(catalog)
    }
}

/// A catalog whose ids contain dots, as real catalogs routinely do.
fn dotted_catalog() -> ModelCatalog {
    ModelCatalog::new(vec![
        CatalogModel::new("glm-4.6", "GLM 4.6", true, Some(200_000)),
        CatalogModel::new("gpt-5.2", "GPT-5.2", true, Some(400_000)),
    ])
}

/// The platform-specific application-data root used by OpenPencil and OpenBot.
#[cfg(target_os = "macos")]
fn app_root(home: &Path, _app: &Path) -> PathBuf {
    home.join("Library").join("Application Support")
}

/// The platform-specific application-data root used by OpenPencil and OpenBot.
#[cfg(not(target_os = "macos"))]
fn app_root(_home: &Path, app: &Path) -> PathBuf {
    app.to_path_buf()
}

fn app_ctx(home: &Path, app: &Path) -> HomeCtx {
    common::home_with(
        home,
        &[
            ("APPDATA", app.display().to_string()),
            ("XDG_CONFIG_HOME", app.display().to_string()),
        ],
    )
}

const KIMI_SEED: &str = concat!(
    "# user config\n",
    "default_model = \"moonshot/kimi-k2\"\n",
    "\n",
    "[mcp_servers.foo]\n",
    "command = \"uvx\"\n",
    "\n",
    "[model_providers.custom]\n",
    "name = \"custom\"\n",
);

#[test]
fn kimi_connect_then_semantic_disconnect() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".kimi-code/config.toml");
    common::write(&path, KIMI_SEED);
    let catalog = common::catalog();

    KimiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "providers.any-converter.type").as_deref(),
        Some("kimi")
    );
    assert_eq!(
        toml::get(&text, "providers.any-converter.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        toml::get(&text, "providers.any-converter.api_key").as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        toml::get(&text, "default_model").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );
    assert_eq!(
        toml::get(&text, "models.any-converter/claude-sonnet-4-5.provider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        toml::get(
            &text,
            "models.any-converter/claude-sonnet-4-5.max_context_size"
        )
        .as_deref(),
        Some("200000")
    );
    assert_eq!(
        toml::get(
            &text,
            "models.any-converter/claude-sonnet-4-5.max_output_size"
        )
        .as_deref(),
        Some("16384")
    );
    assert!(
        toml::get(&text, "models.any-converter/claude-sonnet-4-5.capabilities")
            .unwrap()
            .contains("thinking")
    );
    assert!(
        toml::get(
            &text,
            "models.any-converter/claude-sonnet-4-5.support_efforts"
        )
        .unwrap()
        .contains("medium")
    );
    assert_eq!(
        toml::get(&text, "models.any-converter/claude-sonnet-4-5.display_name").as_deref(),
        Some("Claude Sonnet 4.5")
    );
    assert_eq!(
        KimiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    KimiSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "mcp_servers.foo.command").as_deref(),
        Some("uvx")
    );
    assert_eq!(
        toml::get(&text, "model_providers.custom.name").as_deref(),
        Some("custom")
    );
    assert_eq!(
        toml::get(&text, "default_model").as_deref(),
        Some("moonshot/kimi-k2")
    );
    assert_eq!(toml::get(&text, "providers.any-converter"), None);
    assert!(!text.contains("[models.\"any-converter/"), "{text}");
    assert_eq!(
        KimiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn kimi_golden_roundtrip_is_byte_identical() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".kimi-code/config.toml");
    common::write(&path, KIMI_SEED);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&KimiSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let report = wiring.disconnect(&KimiSpec, &ctx).unwrap();

    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), KIMI_SEED.as_bytes());
}

#[test]
fn kimi_writes_selected_effort_inside_reasoning_tables() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".kimi-code/config.toml");
    common::write(&path, "");
    let catalog = common::catalog();

    KimiSpec
        .connect(&ctx, &wiring_ctx_effort(&catalog, Some("high")))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get_keys(
            &text,
            &[
                "models",
                "any-converter/claude-sonnet-4-5",
                "default_effort"
            ]
        )
        .as_deref(),
        Some("high")
    );
    assert_eq!(
        toml::get_keys(
            &text,
            &["models", "any-converter/claude-haiku-4-5", "default_effort"]
        ),
        None,
        "non-reasoning models must not carry an effort"
    );
    // Kimi never writes a top-level effort key.
    assert_eq!(toml::get(&text, "default_reasoning_effort"), None);

    KimiSpec
        .connect(&ctx, &wiring_ctx_effort(&catalog, Some("")))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get_keys(
            &text,
            &[
                "models",
                "any-converter/claude-sonnet-4-5",
                "default_effort"
            ]
        ),
        None
    );
    assert_eq!(
        KimiSpec.efforts(),
        &["low", "medium", "high"],
        "unexpected effort list"
    );
}

#[test]
fn kimi_legacy_layout_omits_tool_use_and_output_cap() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".kimi/config.toml");
    common::write(&path, "");
    let catalog = common::catalog();

    KimiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&path);
    assert!(!text.contains("max_output_size"), "{text}");
    assert_eq!(
        toml::get_keys(
            &text,
            &["models", "any-converter/claude-sonnet-4-5", "capabilities"]
        )
        .as_deref(),
        Some("[\"thinking\"]")
    );
    assert_eq!(
        toml::get_keys(
            &text,
            &["models", "any-converter/claude-haiku-4-5", "capabilities"]
        ),
        None,
        "legacy kimi-cli rejects tool_use, so non-reasoning models carry none"
    );
}

#[test]
fn kimi_honours_share_dir_for_the_legacy_layout() {
    let home = tempdir().unwrap();
    let share = home.path().join("shared-kimi");
    let path = share.join("config.toml");
    common::write(&path, "");
    let ctx = common::home_with(
        home.path(),
        &[("KIMI_SHARE_DIR", share.display().to_string())],
    );
    let catalog = common::catalog();

    KimiSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "providers.any-converter.type").as_deref(),
        Some("kimi")
    );
    assert!(!text.contains("max_output_size"), "{text}");
}

#[test]
fn kimi_dotted_model_id_is_one_table() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".kimi-code/config.toml");
    common::write(
        &path,
        "[mcp_servers.foo]\ncommand = \"uvx\"\n\n[models.\"user/keep\"]\nprovider = \"user\"\n",
    );
    let catalog = dotted_catalog();

    KimiSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "glm-4.6"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "default_model").as_deref(),
        Some("any-converter/glm-4.6")
    );
    assert!(
        text.contains("[models.\"any-converter/glm-4.6\"]"),
        "{text}"
    );
    assert!(
        !text.contains("[models.\"any-converter/glm-4\"]"),
        "the dot split the id: {text}"
    );
    assert_eq!(
        toml::get_keys(&text, &["models", "any-converter/glm-4.6", "model"]).as_deref(),
        Some("glm-4.6")
    );
    assert_eq!(
        KimiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    KimiSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert!(!text.contains("any-converter/glm-4.6"), "{text}");
    assert_eq!(
        toml::get(&text, "models.user/keep.provider").as_deref(),
        Some("user")
    );
    assert_eq!(
        toml::get(&text, "mcp_servers.foo.command").as_deref(),
        Some("uvx")
    );
    assert_eq!(
        KimiSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn kimi_disconnect_scans_single_quoted_headers() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".kimi-code/config.toml");
    common::write(
        &path,
        "default_model = \"any-converter/glm-4.6\"\n\n[models.'any-converter/glm-4.6']\nprovider = \"any-converter\"\n\n[mcp_servers.foo]\ncommand = \"uvx\"\n",
    );

    KimiSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert!(!text.contains("any-converter"), "{text}");
    assert_eq!(
        toml::get(&text, "mcp_servers.foo.command").as_deref(),
        Some("uvx")
    );
}

#[test]
fn grok_connect_then_semantic_disconnect() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".grok/config.toml");
    let seed = concat!(
        "[models]\n",
        "default = \"grok/grok-4\"\n",
        "\n",
        "[features]\n",
        "campaigns = true\n",
        "\n",
        "[mcp_servers.foo]\n",
        "command = \"uvx\"\n",
    );
    common::write(&path, seed);
    let catalog = common::catalog();

    GrokSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "models.default").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );
    assert_eq!(
        toml::get(&text, "features.campaigns").as_deref(),
        Some("false")
    );
    assert_eq!(
        toml::get(&text, "model.any-converter/claude-sonnet-4-5.api_backend").as_deref(),
        Some("chat_completions")
    );
    assert_eq!(
        toml::get(&text, "model.any-converter/claude-sonnet-4-5.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        toml::get(
            &text,
            "model.any-converter/claude-sonnet-4-5.context_window"
        )
        .as_deref(),
        Some("200000")
    );
    assert!(
        toml::get(
            &text,
            "model.any-converter/claude-sonnet-4-5.reasoning_efforts"
        )
        .unwrap()
        .contains("xhigh")
    );
    assert_eq!(
        GrokSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    GrokSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "mcp_servers.foo.command").as_deref(),
        Some("uvx")
    );
    // The user's previous campaigns value is restored.
    assert_eq!(
        toml::get(&text, "features.campaigns").as_deref(),
        Some("true")
    );
    assert_eq!(toml::get(&text, "models.default"), None);
    assert!(!text.contains("[model.\"any-converter/"), "{text}");
    assert_eq!(
        GrokSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn grok_golden_roundtrip_is_byte_identical() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".grok/config.toml");
    let seed = "[mcp_servers.foo]\ncommand = \"uvx\"\n";
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&GrokSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let report = wiring.disconnect(&GrokSpec, &ctx).unwrap();

    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn grok_creates_and_removes_the_features_table() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".grok/config.toml");
    let seed = "[mcp_servers.foo]\ncommand = \"uvx\"\n";
    common::write(&path, seed);
    let catalog = common::catalog();

    GrokSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "features.campaigns").as_deref(),
        Some("false")
    );

    GrokSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert!(!text.contains("[features]"), "{text}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn grok_dotted_model_id_is_one_table() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".grok/config.toml");
    common::write(
        &path,
        "[models]\ndefault = \"grok/grok-4\"\n\n[model.\"user/keep\"]\nname = \"user\"\n",
    );
    let catalog = dotted_catalog();

    GrokSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "glm-4.6"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "models.default").as_deref(),
        Some("any-converter/glm-4.6")
    );
    assert!(text.contains("[model.\"any-converter/glm-4.6\"]"), "{text}");
    assert_eq!(
        toml::get_keys(&text, &["model", "any-converter/glm-4.6", "model"]).as_deref(),
        Some("glm-4.6")
    );
    assert_eq!(
        GrokSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    GrokSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert!(!text.contains("any-converter/glm-4.6"), "{text}");
    assert_eq!(
        toml::get(&text, "model.user/keep.name").as_deref(),
        Some("user")
    );
    assert_eq!(
        GrokSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn atomcode_connect_then_semantic_disconnect() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".atomcode/config.toml");
    let seed = concat!(
        "default_model = \"old/model\"\n",
        "default_provider = \"old\"\n",
        "\n",
        "[mcp_servers.foo]\n",
        "command = \"uvx\"\n",
    );
    common::write(&path, seed);
    let catalog = common::catalog();

    AtomCodeSpec
        .connect(&ctx, &wiring_ctx_effort(&catalog, Some("high")))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "provider_accounts.any-converter.provider").as_deref(),
        Some("openai-compatible")
    );
    assert_eq!(
        toml::get(&text, "provider_accounts.any-converter.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        toml::get(&text, "default_model").as_deref(),
        Some("any-converter/claude-sonnet-4-5")
    );
    assert_eq!(
        toml::get(&text, "default_provider").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        toml::get(&text, "models.any-converter/claude-sonnet-4-5.account").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        toml::get(
            &text,
            "models.any-converter/claude-sonnet-4-5.supports_vision"
        )
        .as_deref(),
        Some("false")
    );
    assert_eq!(
        toml::get(
            &text,
            "models.any-converter/claude-sonnet-4-5.reasoning_effort"
        )
        .as_deref(),
        Some("high")
    );
    assert_eq!(
        toml::get(
            &text,
            "models.any-converter/claude-haiku-4-5.reasoning_effort"
        ),
        None,
        "non-reasoning models must not carry an effort"
    );
    assert!(
        toml::get(
            &text,
            "models.any-converter/claude-sonnet-4-5.reasoning_effort_levels"
        )
        .unwrap()
        .contains("max")
    );
    assert_eq!(
        AtomCodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    AtomCodeSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "default_model").as_deref(),
        Some("old/model")
    );
    assert_eq!(toml::get(&text, "default_provider").as_deref(), Some("old"));
    assert_eq!(
        toml::get(&text, "mcp_servers.foo.command").as_deref(),
        Some("uvx")
    );
    assert_eq!(toml::get(&text, "provider_accounts.any-converter"), None);
    assert!(!text.contains("[models.\"any-converter/"), "{text}");
    assert_eq!(
        AtomCodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn atomcode_golden_roundtrip_is_byte_identical() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".atomcode/config.toml");
    let seed = concat!(
        "default_model = \"old/model\"\n",
        "default_provider = \"old\"\n",
        "\n",
        "[mcp_servers.foo]\n",
        "command = \"uvx\"\n",
    );
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&AtomCodeSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let report = wiring.disconnect(&AtomCodeSpec, &ctx).unwrap();

    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn atomcode_ignores_a_relative_home() {
    let home = tempdir().unwrap();
    let ctx = common::home_with(
        home.path(),
        &[("ATOMCODE_HOME", "relative/atom".to_owned())],
    );
    let catalog = common::catalog();

    AtomCodeSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    assert!(home.path().join(".atomcode/config.toml").exists());
    assert!(!home.path().join("relative/atom/config.toml").exists());
}

#[test]
fn atomcode_disconnect_scans_single_quoted_headers() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".atomcode/config.toml");
    common::write(
        &path,
        "default_model = \"any-converter/glm-4.6\"\n\n[models.'any-converter/glm-4.6']\naccount = \"any-converter\"\n\n[mcp_servers.foo]\ncommand = \"uvx\"\n",
    );

    AtomCodeSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert!(!text.contains("any-converter"), "{text}");
    assert_eq!(
        toml::get(&text, "mcp_servers.foo.command").as_deref(),
        Some("uvx")
    );
}

#[test]
fn atomcode_dotted_model_id_is_one_table() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".atomcode/config.toml");
    common::write(
        &path,
        "[mcp_servers.foo]\ncommand = \"uvx\"\n\n[models.\"user/keep\"]\naccount = \"user\"\n",
    );
    let catalog = dotted_catalog();

    AtomCodeSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "glm-4.6"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        toml::get(&text, "default_model").as_deref(),
        Some("any-converter/glm-4.6")
    );
    assert!(
        text.contains("[models.\"any-converter/glm-4.6\"]"),
        "{text}"
    );
    assert_eq!(
        toml::get_keys(&text, &["models", "any-converter/glm-4.6", "model"]).as_deref(),
        Some("glm-4.6")
    );
    assert_eq!(
        AtomCodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    AtomCodeSpec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert!(!text.contains("any-converter/glm-4.6"), "{text}");
    assert_eq!(
        toml::get(&text, "models.user/keep.account").as_deref(),
        Some("user")
    );
    assert_eq!(
        AtomCodeSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn pencil_roundtrip_restores_user_providers() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".pencil/models.json");
    let seed =
        "{\n  \"providers\": {\"openai\": {\"name\": \"OpenAI\"}},\n  \"theme\": \"dark\"\n}\n";
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&PencilSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "providers.any-converter.api").as_deref(),
        Some("openai-completions")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.models.0.maxTokens").as_deref(),
        Some("16384")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.models.0.cost.input").as_deref(),
        Some("0")
    );
    // The catalog reports no modalities; text-only is the conservative default.
    assert_eq!(
        json::get(&text, "providers.any-converter.models.0.input").as_deref(),
        Some("[\"text\"]")
    );
    assert_eq!(
        json::get(&text, "providers.any-converter.models.0.compat").as_deref(),
        Some("{}")
    );
    assert_eq!(
        PencilSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&PencilSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
    assert_eq!(
        PencilSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );
}

#[test]
fn openpencil_roundtrip_restores_user_agents() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("openpencil")
        .join("settings.json");
    let seed = concat!(
        "{\n",
        "  \"version\": 1,\n",
        "  \"builtin_agents\": [{\"id\": \"other\", \"kind\": \"builtin\"}],\n",
        "  \"theme\": \"dark\"\n",
        "}\n"
    );
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&OpenPencilSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "builtin_agents.1.id").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&text, "builtin_agents.1.kind").as_deref(),
        Some("openai-compat")
    );
    assert_eq!(
        json::get(&text, "builtin_agents.1.base_url").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "builtin_agents.1.model").as_deref(),
        Some("claude-opus-4-5")
    );
    assert_eq!(
        OpenPencilSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&OpenPencilSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn openpencil_removes_the_file_it_created() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("openpencil")
        .join("settings.json");
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&OpenPencilSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert!(path.exists());
    let report = wiring.disconnect(&OpenPencilSpec, &ctx).unwrap();

    let content = if path.exists() {
        common::read(&path)
    } else {
        "<absent>".to_owned()
    };
    assert!(
        report.kept_backup.is_empty(),
        "{report:?} content={content}"
    );
    assert!(!path.exists());
}

#[test]
fn openpencil_refuses_unknown_versions_and_bad_shapes() {
    let home = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("openpencil")
        .join("settings.json");
    let catalog = common::catalog();

    let version_seed = "{\n  \"version\": 2,\n  \"builtin_agents\": []\n}\n";
    common::write(&path, version_seed);
    let error = OpenPencilSpec
        .connect(&ctx, &wiring_ctx(&catalog))
        .unwrap_err();
    assert!(matches!(error, WiringError::Refused(_)), "{error:?}");
    assert_eq!(common::read(&path), version_seed);

    let shape_seed = "{\n  \"version\": 1,\n  \"builtin_agents\": {\"x\": 1}\n}\n";
    common::write(&path, shape_seed);
    let error = OpenPencilSpec
        .connect(&ctx, &wiring_ctx(&catalog))
        .unwrap_err();
    assert!(matches!(error, WiringError::Refused(_)), "{error:?}");
    assert_eq!(common::read(&path), shape_seed);
}

#[test]
fn openpencil_preserves_our_entry_identity_and_caps_models() {
    let home = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("openpencil")
        .join("settings.json");
    common::write(
        &path,
        "{\n  \"version\": 1,\n  \"builtin_agents\": [{\"id\": \"any-converter\", \"display_name\": \"Mine\", \"enabled\": false}]\n}\n",
    );
    let models = (0..70)
        .map(|index| {
            CatalogModel::new(
                format!("model-{index}"),
                format!("Model {index}"),
                index % 2 == 0,
                Some(100_000),
            )
        })
        .collect();
    let catalog = ModelCatalog::new(models);

    OpenPencilSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "model-0"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "builtin_agents.0.display_name").as_deref(),
        Some("Mine")
    );
    assert_eq!(
        json::get(&text, "builtin_agents.0.enabled").as_deref(),
        Some("false")
    );
    let listed: Vec<serde_json::Value> =
        serde_json::from_str(&json::get(&text, "builtin_agents.0.models").unwrap()).unwrap();
    assert_eq!(listed.len(), 64);
}

#[test]
fn openpencil_omits_models_for_a_single_entry() {
    let home = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("openpencil")
        .join("settings.json");
    let catalog = ModelCatalog::new(vec![CatalogModel::new(
        "only-model",
        "Only",
        true,
        Some(100_000),
    )]);

    OpenPencilSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "only-model"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "builtin_agents.0.model").as_deref(),
        Some("only-model")
    );
    assert_eq!(json::get(&text, "builtin_agents.0.models"), None);
}

#[test]
fn openbot_roundtrip_restores_user_providers() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("OpenBot")
        .join("openbot-custom-providers-v1.json");
    let seed = concat!(
        "{\n",
        "  \"version\": 1,\n",
        "  \"providers\": [{\"id\": \"other\", \"name\": \"Other\", \"baseUrl\": \"https://example.test\", \"models\": [], \"secret\": null}],\n",
        "  \"extra\": true\n",
        "}\n"
    );
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&OpenBotSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "providers.1.id").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&text, "providers.1.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/agent/openbot/v1")
    );
    assert_eq!(
        json::get(&text, "providers.1.secret").as_deref(),
        Some("null")
    );
    assert_eq!(
        json::get(&text, "providers.1.models.0.id").as_deref(),
        Some("claude-opus-4-5")
    );
    assert_eq!(
        OpenBotSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&OpenBotSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn openbot_removes_the_file_it_created() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("OpenBot")
        .join("openbot-custom-providers-v1.json");
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&OpenBotSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert!(path.exists());
    let report = wiring.disconnect(&OpenBotSpec, &ctx).unwrap();

    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert!(!path.exists());
}

#[test]
fn openbot_refuses_unknown_versions_and_non_array_providers() {
    let home = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("OpenBot")
        .join("openbot-custom-providers-v1.json");
    let catalog = common::catalog();

    let version_seed = "{\n  \"version\": 2,\n  \"providers\": []\n}\n";
    common::write(&path, version_seed);
    let error = OpenBotSpec
        .connect(&ctx, &wiring_ctx(&catalog))
        .unwrap_err();
    assert!(matches!(error, WiringError::Refused(_)), "{error:?}");
    assert_eq!(common::read(&path), version_seed);

    let shape_seed = "{\n  \"version\": 1,\n  \"providers\": {}\n}\n";
    common::write(&path, shape_seed);
    let error = OpenBotSpec
        .connect(&ctx, &wiring_ctx(&catalog))
        .unwrap_err();
    assert!(matches!(error, WiringError::Refused(_)), "{error:?}");
    assert_eq!(common::read(&path), shape_seed);
}

#[test]
fn openbot_filters_unacceptable_model_ids() {
    let home = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("OpenBot")
        .join("openbot-custom-providers-v1.json");
    let long_id = "a".repeat(129);
    let catalog = ModelCatalog::new(vec![
        CatalogModel::new("bad id", "Bad", false, Some(1_000)),
        CatalogModel::new("ok.1", "OK", false, Some(500)),
        CatalogModel::new("ok.1", "Duplicate", false, Some(2_000)),
        CatalogModel::new("", "Empty", false, None),
        CatalogModel::new(long_id, "Long", false, Some(1_000)),
        CatalogModel::new("tool[1m]", "Tool", false, None),
    ]);

    OpenBotSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "ok.1"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "providers.0.models.0.id").as_deref(),
        Some("ok.1")
    );
    assert_eq!(
        json::get(&text, "providers.0.models.0.contextWindow").as_deref(),
        Some("1024")
    );
    assert_eq!(
        json::get(&text, "providers.0.models.1.id").as_deref(),
        Some("tool[1m]")
    );
    assert_eq!(json::get(&text, "providers.0.models.2.id"), None);
}

#[test]
fn openbot_caps_the_model_list() {
    let home = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("OpenBot")
        .join("openbot-custom-providers-v1.json");
    let catalog = ModelCatalog::new(
        (0..70)
            .map(|index| {
                CatalogModel::new(
                    format!("model-{index}"),
                    format!("Model {index}"),
                    false,
                    Some(10_000),
                )
            })
            .collect(),
    );

    OpenBotSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "model-0"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "providers.0.models.63.id").as_deref(),
        Some("model-63")
    );
    assert_eq!(json::get(&text, "providers.0.models.64.id"), None);
}

#[test]
fn openbot_preserves_our_provider_identity_fields() {
    let home = tempdir().unwrap();
    let app = home.path().join("appdata");
    let ctx = app_ctx(home.path(), &app);
    let path = app_root(home.path(), &app)
        .join("OpenBot")
        .join("openbot-custom-providers-v1.json");
    common::write(
        &path,
        "{\n  \"version\": 1,\n  \"providers\": [{\"id\": \"any-converter\", \"name\": \"Mine\", \"harness\": \"custom-harness\", \"secret\": \"s3cret\", \"models\": []}]\n}\n",
    );
    let catalog = common::catalog();

    OpenBotSpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "providers.0.name").as_deref(),
        Some("Mine")
    );
    assert_eq!(
        json::get(&text, "providers.0.harness").as_deref(),
        Some("custom-harness")
    );
    assert_eq!(
        json::get(&text, "providers.0.secret").as_deref(),
        Some("s3cret")
    );
}

#[test]
fn craft_agents_roundtrip_restores_user_connection() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".craft-agent/config.json");
    let seed = concat!(
        "{\n",
        "  \"llmConnections\": [{\"slug\": \"user\", \"name\": \"User\"}],\n",
        "  \"defaultLlmConnection\": \"user\",\n",
        "  \"theme\": \"dark\"\n",
        "}\n"
    );
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&CraftAgentsSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "llmConnections.1.slug").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&text, "llmConnections.1.providerType").as_deref(),
        Some("pi_compat")
    );
    assert_eq!(
        json::get(&text, "llmConnections.1.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/agent/craft-agents/v1")
    );
    assert_eq!(
        json::get(&text, "llmConnections.1.defaultModel").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&text, "llmConnections.1.models.0.shortName").as_deref(),
        Some("Claude Opus 4.5")
    );
    assert_eq!(
        json::get(&text, "llmConnections.1.models.0.description").as_deref(),
        Some("Claude Opus 4.5")
    );
    assert!(json::get(&text, "llmConnections.1.createdAt").is_some());
    assert_eq!(
        json::get(&text, "defaultLlmConnection").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        CraftAgentsSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&CraftAgentsSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn craft_agents_adds_the_default_model_when_absent() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".craft-agent/config.json");
    let catalog = common::catalog();

    CraftAgentsSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "custom/default"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "llmConnections.0.defaultModel").as_deref(),
        Some("custom/default")
    );
    assert_eq!(
        json::get(&text, "llmConnections.0.models.5.id").as_deref(),
        Some("custom/default")
    );
    assert_eq!(
        json::get(&text, "llmConnections.0.models.5.shortName").as_deref(),
        Some("custom/default")
    );
}

#[test]
fn craft_agents_preserves_our_connection_name_and_timestamp() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".craft-agent/config.json");
    common::write(
        &path,
        "{\n  \"llmConnections\": [{\"slug\": \"any-converter\", \"name\": \"Mine\", \"createdAt\": 123}],\n  \"defaultLlmConnection\": \"any-converter\"\n}\n",
    );
    let catalog = common::catalog();

    CraftAgentsSpec
        .connect(&ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "llmConnections.0.name").as_deref(),
        Some("Mine")
    );
    assert_eq!(
        json::get(&text, "llmConnections.0.createdAt").as_deref(),
        Some("123")
    );
}

#[test]
fn tuanjie_roundtrip_supports_arrays() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".codely/settings.json");
    let seed = concat!(
        "{\n",
        "  \"customModelProviders\": [{\"id\": \"user\", \"name\": \"User\"}],\n",
        "  \"customModels\": [{\"id\": \"user\", \"model\": \"m\"}],\n",
        "  \"theme\": \"dark\"\n",
        "}\n"
    );
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&TuanjieCoworkSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "customModelProviders.1.id").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&text, "customModelProviders.1.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "customModels.1.model").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        TuanjieCoworkSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&TuanjieCoworkSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn tuanjie_roundtrip_supports_maps() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".codely/settings.json");
    let seed = concat!(
        "{\n",
        "  \"customModelProviders\": {\"user\": {\"id\": \"user\"}},\n",
        "  \"customModels\": {\"user\": {\"id\": \"user\", \"model\": \"m\"}},\n",
        "  \"theme\": \"dark\"\n",
        "}\n"
    );
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&TuanjieCoworkSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "customModelProviders.any-converter.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "customModels.any-converter.model").as_deref(),
        Some("claude-sonnet-4-5")
    );

    let report = wiring.disconnect(&TuanjieCoworkSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn tuanjie_merges_into_an_existing_our_entry() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".codely/settings.json");
    common::write(
        &path,
        concat!(
            "{\n",
            "  \"customModelProviders\": [{\"id\": \"any-converter\", \"name\": \"My Gateway\", \"baseUrl\": \"http://old\"}],\n",
            "  \"customModels\": [{\"id\": \"any-converter\", \"model\": \"old-model\", \"roles\": [\"planner\"]}]\n",
            "}\n"
        ),
    );
    let catalog = common::catalog();

    TuanjieCoworkSpec
        .connect(&ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "customModelProviders.0.name").as_deref(),
        Some("My Gateway")
    );
    assert_eq!(
        json::get(&text, "customModelProviders.0.baseUrl").as_deref(),
        Some("http://127.0.0.1:8787/v1")
    );
    assert_eq!(
        json::get(&text, "customModels.0.model").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        json::get(&text, "customModels.0.roles").as_deref(),
        Some("[\"planner\"]")
    );
}

#[test]
fn workbuddy_semantic_disconnect_keeps_user_models() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".workbuddy/models.json");
    let seed = concat!(
        "{\n",
        "  \"models\": [{\"id\": \"user-model\", \"vendor\": \"other\"}],\n",
        "  \"availableModels\": [\"user-model\"],\n",
        "  \"theme\": \"dark\"\n",
        "}\n"
    );
    common::write(&path, seed);
    let catalog = common::catalog();
    let spec = workbuddy();

    spec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "models.1.id").as_deref(),
        Some("claude-opus-4-5")
    );
    assert_eq!(
        json::get(&text, "models.1.vendor").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&text, "models.1.url").as_deref(),
        Some("http://127.0.0.1:8787/v1/chat/completions")
    );
    // Output limits are unknown to the catalog, so the key is omitted.
    assert_eq!(json::get(&text, "models.1.maxOutputTokens"), None);
    assert_eq!(
        json::get(&text, "models.1.reasoning.defaultEffort").as_deref(),
        Some("medium")
    );
    // Reasoning is per model and does not appear on non-reasoning entries.
    assert_eq!(json::get(&text, "models.3.reasoning"), None);
    // A non-empty availableModels list is a whitelist; our ids are appended.
    assert_eq!(
        json::get(&text, "availableModels.0").as_deref(),
        Some("user-model")
    );
    assert_eq!(
        json::get(&text, "availableModels.1").as_deref(),
        Some("claude-opus-4-5")
    );
    assert_eq!(
        spec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    spec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "models.0.id").as_deref(),
        Some("user-model")
    );
    assert_eq!(json::get(&text, "models.1.id"), None);
    assert_eq!(
        json::get(&text, "availableModels.0").as_deref(),
        Some("user-model")
    );
    assert_eq!(json::get(&text, "availableModels.1"), None);
    assert_eq!(json::get(&text, "theme").as_deref(), Some("dark"));
}

#[test]
fn workbuddy_golden_roundtrip_is_byte_identical() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".workbuddy/models.json");
    let seed = "{\n  \"models\": [{\"id\": \"user-model\", \"vendor\": \"other\"}]\n}\n";
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());
    let spec = workbuddy();

    wiring.connect(&spec, &ctx, &wiring_ctx(&catalog)).unwrap();
    let report = wiring.disconnect(&spec, &ctx).unwrap();

    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn workbuddy_ai_uses_its_own_directory() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".workbuddy-ai/models.json");
    let catalog = common::catalog();
    let spec = workbuddy_ai();

    assert_eq!(spec.id(), "workbuddy-ai");
    spec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    assert!(path.exists());
    assert_eq!(
        spec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );
    spec.disconnect(&ctx).unwrap();
    assert_eq!(json::get(&common::read(&path), "models"), None);
}

#[test]
fn workbuddy_handles_a_bare_array_root() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".workbuddy/models.json");
    common::write(
        &path,
        "[\n  {\"id\": \"user-model\", \"vendor\": \"other\"}\n]\n",
    );
    let catalog = common::catalog();
    let spec = workbuddy();

    spec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&path);
    assert_eq!(json::get(&text, "0.id").as_deref(), Some("user-model"));
    assert_eq!(json::get(&text, "1.id").as_deref(), Some("claude-opus-4-5"));
    assert_eq!(
        spec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    spec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert_eq!(json::get(&text, "0.id").as_deref(), Some("user-model"));
    assert_eq!(json::get(&text, "1.id"), None);
}

#[test]
fn workbuddy_uses_a_conservative_context_default() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".workbuddy/models.json");
    let catalog = ModelCatalog::new(vec![
        CatalogModel::new("plain", "Plain", false, None),
        CatalogModel::new("thinking", "Thinking", true, None),
    ]);
    let spec = workbuddy();

    spec.connect(&ctx, &wiring_ctx_default(&catalog, "plain"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "models.0.maxInputTokens").as_deref(),
        Some("200000")
    );
    assert_eq!(json::get(&text, "models.0.reasoning"), None);
    assert!(json::get(&text, "models.1.reasoning").is_some());
}

#[test]
fn workbuddy_recognizes_entries_by_our_api_key() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".workbuddy/models.json");
    common::write(
        &path,
        concat!(
            "{\n",
            "  \"models\": [\n",
            "    {\"id\": \"user-model\", \"vendor\": \"other\"},\n",
            "    {\"id\": \"changed-model\", \"vendor\": \"other\", \"apiKey\": \"acw_test_agent_key\"},\n",
            "    {\"id\": \"ours-model\", \"vendor\": \"any-converter\", \"apiKey\": \"acw_test_agent_key\"}\n",
            "  ]\n",
            "}\n"
        ),
    );
    let catalog = common::catalog();
    let spec = workbuddy();

    spec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let during = common::read(&path);
    assert_eq!(
        json::get(&during, "models.0.id").as_deref(),
        Some("user-model")
    );
    assert_eq!(
        json::get(&during, "models.1.id").as_deref(),
        Some("claude-opus-4-5")
    );

    spec.disconnect(&ctx).unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "models.0.id").as_deref(),
        Some("user-model")
    );
    assert_eq!(json::get(&text, "models.1.id"), None);
    assert!(!text.contains("changed-model"), "{text}");
}

#[test]
fn codebuddy_semantic_disconnect_restores_selected_model() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let models = home.path().join(".codebuddy/models.json");
    let settings = home.path().join(".codebuddy/settings.json");
    common::write(
        &models,
        "{\n  \"models\": [{\"id\": \"user-model\", \"vendor\": \"other\"}]\n}\n",
    );
    common::write(
        &settings,
        "{\n  \"model\": \"old-model\",\n  \"theme\": \"dark\"\n}\n",
    );
    let catalog = common::catalog();

    CodeBuddySpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    assert_eq!(
        json::get(&common::read(&models), "models.1.vendor").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        json::get(&common::read(&settings), "model").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        CodeBuddySpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    CodeBuddySpec.disconnect(&ctx).unwrap();
    assert_eq!(
        json::get(&common::read(&settings), "model").as_deref(),
        Some("old-model")
    );
    assert_eq!(json::get(&common::read(&models), "models.1.id"), None);
    assert_eq!(
        json::get(&common::read(&models), "models.0.id").as_deref(),
        Some("user-model")
    );
}

#[test]
fn codebuddy_golden_roundtrip_is_byte_identical() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let models = home.path().join(".codebuddy/models.json");
    let settings = home.path().join(".codebuddy/settings.json");
    let models_seed = "{\n  \"models\": [{\"id\": \"user-model\", \"vendor\": \"other\"}]\n}\n";
    let settings_seed = "{\n  \"model\": \"old-model\",\n  \"theme\": \"dark\"\n}\n";
    common::write(&models, models_seed);
    common::write(&settings, settings_seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&CodeBuddySpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let report = wiring.disconnect(&CodeBuddySpec, &ctx).unwrap();

    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&models).unwrap(), models_seed.as_bytes());
    assert_eq!(fs::read(&settings).unwrap(), settings_seed.as_bytes());
}

#[test]
fn codebuddy_appends_available_models_and_omits_unknown_output() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let models = home.path().join(".codebuddy/models.json");
    let settings = home.path().join(".codebuddy/settings.json");
    common::write(
        &models,
        "{\n  \"models\": [{\"id\": \"user-model\", \"vendor\": \"other\"}],\n  \"availableModels\": [\"user-model\"]\n}\n",
    );
    common::write(&settings, "{\n  \"model\": \"old-model\"\n}\n");
    let catalog = common::catalog();

    CodeBuddySpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&models);
    assert_eq!(
        json::get(&text, "availableModels.0").as_deref(),
        Some("user-model")
    );
    assert_eq!(
        json::get(&text, "availableModels.1").as_deref(),
        Some("claude-opus-4-5")
    );
    assert_eq!(json::get(&text, "models.1.maxOutputTokens"), None);
    assert_eq!(json::get(&text, "models.3.reasoning"), None);
    assert!(json::get(&text, "models.1.reasoning").is_some());

    CodeBuddySpec.disconnect(&ctx).unwrap();
    let text = common::read(&models);
    assert_eq!(
        json::get(&text, "availableModels.0").as_deref(),
        Some("user-model")
    );
    assert_eq!(json::get(&text, "availableModels.1"), None);
}

#[test]
fn codebuddy_handles_a_bare_array_root() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let models = home.path().join(".codebuddy/models.json");
    let settings = home.path().join(".codebuddy/settings.json");
    common::write(
        &models,
        "[\n  {\"id\": \"user-model\", \"vendor\": \"other\"}\n]\n",
    );
    common::write(&settings, "{\n  \"model\": \"old-model\"\n}\n");
    let catalog = common::catalog();

    CodeBuddySpec.connect(&ctx, &wiring_ctx(&catalog)).unwrap();
    let text = common::read(&models);
    assert_eq!(json::get(&text, "0.id").as_deref(), Some("user-model"));
    assert_eq!(json::get(&text, "1.id").as_deref(), Some("claude-opus-4-5"));

    CodeBuddySpec.disconnect(&ctx).unwrap();
    let text = common::read(&models);
    assert_eq!(json::get(&text, "0.id").as_deref(), Some("user-model"));
    assert_eq!(json::get(&text, "1.id"), None);
}

#[test]
fn claude_science_detects_binary_and_exposes_a_launcher() {
    let home = tempdir().unwrap();
    let bin = home.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    #[cfg(windows)]
    let name = "claude-science.exe";
    #[cfg(not(windows))]
    let name = "claude-science";
    fs::write(bin.join(name), "").unwrap();
    let ctx = common::home_with(
        home.path(),
        &[
            ("PATH", bin.display().to_string()),
            ("PATHEXT", ".EXE".to_owned()),
        ],
    );

    assert!(matches!(
        ClaudeScienceSpec.detect(&ctx),
        DetectResult::Detected { .. }
    ));
    assert!(ClaudeScienceSpec.config_files(&ctx).is_empty());
    assert!(
        ClaudeScienceSpec
            .connect(&ctx, &wiring_ctx(&common::catalog()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ClaudeScienceSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Disconnected
    );

    let launch = ClaudeScienceSpec
        .launch(
            &ctx,
            common::GATEWAY,
            common::AGENT_KEY,
            common::DEFAULT_MODEL,
        )
        .expect("claude-science needs a launcher");
    assert!(launch.contains(common::GATEWAY), "{launch}");
    // The app keeps its own Claude sign-in; no auth token is exported.
    assert!(!launch.contains("ANTHROPIC_AUTH_TOKEN"), "{launch}");
    assert!(!launch.contains(common::AGENT_KEY), "{launch}");
    assert!(launch.contains("claude-science stop"), "{launch}");
    assert!(
        launch.contains("claude-science serve --detached --no-browser"),
        "{launch}"
    );
}

#[test]
fn openmausbot_roundtrip_restores_user_instances() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".openmausbot/config.json");
    let seed = concat!(
        "{\n",
        "  \"instances\": {\"user\": {\"driver\": \"claudeAgent\"}},\n",
        "  \"theme\": \"dark\"\n",
        "}\n"
    );
    common::write(&path, seed);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&OpenMausBotSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();
    let text = common::read(&path);
    // The saved map is the whole fleet: the user's engine survives.
    assert_eq!(
        json::get(&text, "instances.user.driver").as_deref(),
        Some("claudeAgent")
    );
    assert_eq!(
        json::get(&text, "instances.any-converter.driver").as_deref(),
        Some("claudeAgent")
    );
    assert_eq!(
        json::get(
            &text,
            "instances.any-converter.environment.ANTHROPIC_BASE_URL"
        )
        .as_deref(),
        Some("http://127.0.0.1:8787")
    );
    assert_eq!(
        json::get(
            &text,
            "instances.any-converter.environment.ANTHROPIC_AUTH_TOKEN"
        )
        .as_deref(),
        Some(common::AGENT_KEY)
    );
    assert_eq!(
        json::get(&text, "instances.any-converter.config.endpointModels.0").as_deref(),
        Some("claude-opus-4-5")
    );
    assert_eq!(
        json::get(&text, "instances.any-converter.config.endpointModels.2").as_deref(),
        Some("claude-haiku-4-5")
    );
    // Every catalog id is included, not only Claude-named ones.
    assert_eq!(
        json::get(&text, "instances.any-converter.config.endpointModels.3").as_deref(),
        Some("gpt-5-codex")
    );
    assert_eq!(
        json::get(&text, "instances.any-converter.config.endpointModels.4").as_deref(),
        Some("gemini-3-pro")
    );
    // The first endpoint model, in its written spelling.
    assert_eq!(
        json::get(&text, "instances.any-converter.environment.ANTHROPIC_MODEL").as_deref(),
        Some("claude-opus-4-5")
    );
    assert!(
        json::get(&text, "instances.any-converter.config.configDir")
            .unwrap()
            .ends_with("providers/any-converter")
            || json::get(&text, "instances.any-converter.config.configDir")
                .unwrap()
                .ends_with("providers\\any-converter")
    );
    assert_eq!(
        OpenMausBotSpec.read_state(&ctx, common::GATEWAY),
        WiredState::Connected
    );

    let report = wiring.disconnect(&OpenMausBotSpec, &ctx).unwrap();
    assert!(report.kept_backup.is_empty(), "{report:?}");
    assert_eq!(fs::read(&path).unwrap(), seed.as_bytes());
}

#[test]
fn openmausbot_ignores_a_relative_data_dir() {
    let home = tempdir().unwrap();
    let ctx = common::home_with(home.path(), &[("OMB_DATA_DIR", "relative/omb".to_owned())]);
    let catalog = common::catalog();

    OpenMausBotSpec
        .connect(&ctx, &wiring_ctx(&catalog))
        .unwrap();
    assert!(home.path().join(".openmausbot/config.json").exists());
    assert!(!home.path().join("relative/omb/config.json").exists());
}

#[test]
fn openmausbot_filters_ids_and_skips_when_none_remain() {
    let home = tempdir().unwrap();
    let ctx = common::home(home.path());
    let path = home.path().join(".openmausbot/config.json");
    let catalog = ModelCatalog::new(vec![
        CatalogModel::new("bad id", "Bad", false, None),
        CatalogModel::new("good-model", "Good", false, None),
        CatalogModel::new("x[1m]", "Tool", false, None),
    ]);

    OpenMausBotSpec
        .connect(&ctx, &wiring_ctx_default(&catalog, "good-model"))
        .unwrap();
    let text = common::read(&path);
    assert_eq!(
        json::get(&text, "instances.any-converter.config.endpointModels.0").as_deref(),
        Some("good-model")
    );
    assert_eq!(
        json::get(&text, "instances.any-converter.config.endpointModels.1").as_deref(),
        Some("x[1m]")
    );
    assert_eq!(
        json::get(&text, "instances.any-converter.config.endpointModels.2"),
        None
    );

    let none = ModelCatalog::new(vec![CatalogModel::new("bad id", "Bad", false, None)]);
    let untouched = "{\n  \"instances\": {}\n}\n";
    common::write(&path, untouched);
    let written = OpenMausBotSpec
        .connect(&ctx, &wiring_ctx_default(&none, "bad id"))
        .unwrap();
    assert!(written.is_empty(), "{written:?}");
    assert_eq!(common::read(&path), untouched);
}
