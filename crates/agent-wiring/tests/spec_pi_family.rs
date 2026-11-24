#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use agent_wiring::edit::json;
use agent_wiring::spec::pi_family::{mu, omo, prime_agent};
use agent_wiring::spec::{AgentSpec, DetectResult, WiringCtx};
use tempfile::tempdir;

fn wiring_ctx<'a>(catalog: &'a agent_wiring::ModelCatalog) -> WiringCtx<'a> {
    WiringCtx {
        gateway_url: common::GATEWAY,
        agent_key: common::AGENT_KEY,
        catalog,
        default_model: "claude-sonnet-4-5",
        effort: Some("high"),
        roles: &common::NO_ROLES,
    }
}

#[test]
fn omo_writes_pi_shaped_models_and_scope() {
    let dir = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".omo/agent/settings.json");
    let models = dir.path().join(".omo/agent/models.json");
    common::write(&settings, "{\"enabledModels\": [\"other/model\"]}\n");
    common::write(&models, "{}\n");
    let catalog = common::catalog();

    omo().connect(&ctx, &wiring_ctx(&catalog)).unwrap();

    let models_text = common::read(&models);
    assert_eq!(
        json::get(&models_text, "providers.any-converter.api").as_deref(),
        Some("openai-completions")
    );
    assert_eq!(
        json::get(
            &models_text,
            "providers.any-converter.compat.sendSessionAffinityHeaders"
        )
        .as_deref(),
        Some("true")
    );
    // Array-shaped models with an `id`, `contextWindow`, not an id-keyed map.
    assert_eq!(
        json::get(&models_text, "providers.any-converter.models.0.id").as_deref(),
        Some("claude-opus-4-5")
    );
    assert_eq!(
        json::get(
            &models_text,
            "providers.any-converter.models.0.contextWindow"
        )
        .as_deref(),
        Some("200000")
    );
    assert_eq!(
        json::get(&models_text, "providers.any-converter.models.0.context"),
        None
    );

    // The default joins the fork's enabledModels list.
    assert_eq!(
        json::get(&common::read(&settings), "enabledModels").as_deref(),
        Some("[\"other/model\",\"any-converter/claude-sonnet-4-5\"]")
    );

    omo().disconnect(&ctx).unwrap();
    assert_eq!(
        json::get(&common::read(&settings), "enabledModels").as_deref(),
        Some("[\"other/model\"]")
    );
}

#[test]
fn pi_family_ignores_relative_env_dirs() {
    let dir = tempdir().unwrap();
    let agent = dir.path().join(".omo/agent");
    fs::create_dir_all(&agent).unwrap();
    let ctx = common::home_with(
        dir.path(),
        &[("OMO_CODING_AGENT_DIR", "rel/omo".to_owned())],
    );
    assert_eq!(
        omo().detect(&ctx),
        DetectResult::Detected { config_dir: agent }
    );
}

#[test]
fn pi_family_accepts_absolute_env_dirs() {
    let dir = tempdir().unwrap();
    let custom = dir.path().join("custom-prime");
    fs::create_dir_all(&custom).unwrap();
    let ctx = common::home_with(
        dir.path(),
        &[("PRIME_AGENT_CODING_AGENT_DIR", custom.display().to_string())],
    );
    assert_eq!(
        prime_agent().detect(&ctx),
        DetectResult::Detected { config_dir: custom }
    );
}

#[test]
fn mu_falls_back_to_legacy_kyrn_dir() {
    let dir = tempdir().unwrap();
    let kyrn = dir.path().join(".kyrn/agent");
    fs::create_dir_all(&kyrn).unwrap();
    assert!(!dir.path().join(".mu").exists());
    let ctx = common::home(dir.path());
    assert_eq!(
        mu().detect(&ctx),
        DetectResult::Detected { config_dir: kyrn }
    );
}

#[test]
fn mu_prefers_dot_mu_when_both_exist() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join(".mu/agent");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(dir.path().join(".kyrn/agent")).unwrap();
    let ctx = common::home(dir.path());
    assert_eq!(
        mu().detect(&ctx),
        DetectResult::Detected {
            config_dir: primary
        }
    );
}
