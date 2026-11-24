//! Claude Science CLI specification.
//!
//! Claude Science takes its endpoint only from the environment; no
//! configuration file holds it. "Connecting" therefore writes nothing and the
//! wiring is carried by [`AgentSpec::launch`], which exports only
//! `ANTHROPIC_BASE_URL` before restarting the daemon in the background. The app
//! keeps using its own Claude sign-in, so no auth token is exported.

use std::path::PathBuf;

use super::{AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, join_relative};
use crate::error::WiringError;

/// Claude Science CLI wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeScienceSpec;

impl AgentSpec for ClaudeScienceSpec {
    fn id(&self) -> &'static str {
        "claude-science"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        &[]
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        if bin_in_path(ctx, "claude-science") {
            DetectResult::Detected {
                config_dir: join_relative(&ctx.home, ".claude-science"),
            }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, _ctx: &HomeCtx) -> Vec<PathBuf> {
        Vec::new()
    }

    fn connect(
        &self,
        _ctx: &HomeCtx,
        _wiring: &WiringCtx<'_>,
    ) -> Result<Vec<PathBuf>, WiringError> {
        // The endpoint lives in the launch environment only.
        Ok(Vec::new())
    }

    fn disconnect(&self, _ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        Ok(Vec::new())
    }

    fn read_state(&self, _ctx: &HomeCtx, _gateway_url: &str) -> WiredState {
        WiredState::Disconnected
    }

    fn launch(
        &self,
        _ctx: &HomeCtx,
        gateway_url: &str,
        _agent_key: &str,
        _model: &str,
    ) -> Option<String> {
        Some(if cfg!(windows) {
            // PowerShell: the daemon is restarted after the environment is set.
            format!(
                "$env:ANTHROPIC_BASE_URL=\"{gateway_url}\"; claude-science stop; claude-science serve --detached --no-browser"
            )
        } else {
            // POSIX: export the environment for the foreground command.
            format!(
                "claude-science stop && ANTHROPIC_BASE_URL=\"{gateway_url}\" claude-science serve --detached --no-browser"
            )
        })
    }
}
