#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::PathBuf;

use agent_wiring::backup::{Applied, Stash};
use agent_wiring::spec::claude::ClaudeSpec;
use agent_wiring::spec::{AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, all_specs};
use agent_wiring::wiring::Wiring;
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
fn registry_contains_all_four_agents() {
    let ids: Vec<&str> = all_specs().iter().map(|spec| spec.id()).collect();
    assert_eq!(ids, vec!["claude-code", "codex", "gemini-cli", "pi"]);
}

#[test]
fn connect_records_applied_values_and_stash() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".claude/settings.json");
    common::write(
        &settings,
        "{\"env\": {\"ANTHROPIC_BASE_URL\": \"https://api.anthropic.com\"}}",
    );
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    wiring
        .connect(&ClaudeSpec, &ctx, &wiring_ctx(&catalog))
        .unwrap();

    let applied = Applied::load(&data.path().join("agent_applied.json"));
    assert_eq!(
        applied
            .recorded("claude-code")
            .get("env.ANTHROPIC_BASE_URL"),
        Some(&common::GATEWAY.to_owned())
    );
    let stash = Stash::load(&data.path().join("agent_stash.json"));
    assert_eq!(
        stash
            .old_values("claude-code")
            .get("env.ANTHROPIC_BASE_URL"),
        Some(&"https://api.anthropic.com".to_owned())
    );
    assert_eq!(wiring.check_drift(&ClaudeSpec, &ctx), WiredState::Connected);
}

#[test]
fn disconnect_without_stash_warns_but_keeps_user_file() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let settings = dir.path().join(".claude/settings.json");
    common::write(&settings, "{\n  \"theme\": \"dark\"\n}\n");
    let wiring = Wiring::new(data.path());

    let report = wiring.disconnect(&ClaudeSpec, &ctx).unwrap();

    assert!(report.warnings.contains(&"stash_missing".to_owned()));
    assert!(report.kept_backup.is_empty());
    assert_eq!(
        agent_wiring::edit::json::get(&common::read(&settings), "theme").as_deref(),
        Some("dark")
    );
}

#[cfg(unix)]
#[test]
fn backup_failure_aborts_connect_before_any_write() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let claude_dir = dir.path().join(".claude");
    let settings = claude_dir.join("settings.json");
    let original = "{\n  \"theme\": \"dark\"\n}\n";
    common::write(&settings, original);
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    let original_permissions = fs::metadata(&claude_dir).unwrap().permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_mode(0o555);
    fs::set_permissions(&claude_dir, readonly).unwrap();

    let result = wiring.connect(&ClaudeSpec, &ctx, &wiring_ctx(&catalog));

    fs::set_permissions(&claude_dir, original_permissions).unwrap();
    assert!(result.is_err());
    assert_eq!(common::read(&settings), original);
    assert_eq!(
        agent_wiring::edit::json::get(&common::read(&settings), "env.ANTHROPIC_BASE_URL"),
        None
    );
}

#[test]
fn mid_connect_failure_restores_already_written_files() {
    let dir = tempdir().unwrap();
    let data = tempdir().unwrap();
    let ctx = common::home(dir.path());
    let first = dir.path().join("first.json");
    let second = dir.path().join("second.json");
    common::write(&first, "original");
    common::write(&second, "second-original");
    let catalog = common::catalog();
    let wiring = Wiring::new(data.path());

    let result = wiring.connect(&FailingSpec, &ctx, &wiring_ctx(&catalog));

    assert!(result.is_err());
    assert_eq!(common::read(&first), "original");
    assert_eq!(common::read(&second), "second-original");
}

struct FailingSpec;

impl AgentSpec for FailingSpec {
    fn id(&self) -> &'static str {
        "failing"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        &[]
    }

    fn detect(&self, _ctx: &HomeCtx) -> DetectResult {
        DetectResult::Absent
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![ctx.home.join("first.json"), ctx.home.join("second.json")]
    }

    fn connect(
        &self,
        ctx: &HomeCtx,
        _wiring: &WiringCtx<'_>,
    ) -> Result<Vec<PathBuf>, agent_wiring::WiringError> {
        let first = ctx.home.join("first.json");
        agent_wiring::atomic::write_atomic(&first, b"changed")?;
        Err(agent_wiring::WiringError::Refused("simulated".to_owned()))
    }

    fn disconnect(&self, _ctx: &HomeCtx) -> Result<Vec<PathBuf>, agent_wiring::WiringError> {
        Ok(Vec::new())
    }

    fn read_state(&self, _ctx: &HomeCtx) -> WiredState {
        WiredState::Disconnected
    }
}
