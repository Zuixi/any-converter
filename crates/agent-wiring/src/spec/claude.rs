//! Claude Code specification (`~/.claude/settings.json`, JSONC).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gw_base_match, load_stash,
    read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Claude Code spec.
pub const OUR_KEYS: &[&str] = &[
    "env.ANTHROPIC_BASE_URL",
    "env.ANTHROPIC_AUTH_TOKEN",
    "env.ANTHROPIC_MODEL",
    "env.ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "env.ANTHROPIC_DEFAULT_SONNET_MODEL",
    "env.ANTHROPIC_DEFAULT_OPUS_MODEL",
    "env.ANTHROPIC_API_KEY",
];

/// Claude Code wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeSpec;

impl ClaudeSpec {
    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        ctx.home.join(".claude").join("settings.json")
    }
}

impl AgentSpec for ClaudeSpec {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = ctx.home.join(".claude");
        let settings = Self::settings_path(ctx);
        if settings.exists() || config_dir.is_dir() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let current = read_config(&path)?.unwrap_or_default();

        if json::get(&current, "apiKeyHelper").is_some() {
            return Err(WiringError::Refused(
                "apiKeyHelper is configured; remove it before connecting".to_owned(),
            ));
        }

        let mut stash = load_stash(ctx);
        let old = OUR_KEYS
            .iter()
            .filter_map(|key| json::get(&current, key).map(|value| ((*key).to_owned(), value)))
            .collect::<BTreeMap<_, _>>();
        record_old_values(&mut stash, self.id(), old);

        let mut kvs = vec![
            KV::new("env.ANTHROPIC_BASE_URL", json!(wiring.gateway_url)),
            KV::new("env.ANTHROPIC_AUTH_TOKEN", json!(wiring.agent_key)),
            KV::new("env.ANTHROPIC_MODEL", json!(wiring.default_model)),
        ];

        if let Some((sonnet, haiku, opus)) = wiring.catalog.tier_models() {
            if let Some(model) = sonnet {
                kvs.push(KV::new(
                    "env.ANTHROPIC_DEFAULT_SONNET_MODEL",
                    json!(model.id),
                ));
            }
            if let Some(model) = haiku {
                kvs.push(KV::new(
                    "env.ANTHROPIC_DEFAULT_HAIKU_MODEL",
                    json!(model.id),
                ));
            }
            if let Some(model) = opus {
                kvs.push(KV::new("env.ANTHROPIC_DEFAULT_OPUS_MODEL", json!(model.id)));
            }
        }

        let mut text = json::set(&current, &kvs)?;
        text = json::del(&text, &["env.ANTHROPIC_API_KEY"])?;
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let Some(current) = read_config(&path)? else {
            let mut stash = load_stash(ctx);
            stash.take_namespace(self.id());
            save_stash(ctx, &stash)?;
            return Ok(Vec::new());
        };

        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut sets = Vec::new();
        let mut deletes = Vec::new();
        for key in OUR_KEYS {
            match old.get(*key) {
                Some(value) if !value.is_empty() => sets.push(KV::new(*key, json!(value))),
                _ => deletes.push(*key),
            }
        }

        let mut text = json::set(&current, &sets)?;
        text = json::del(&text, &deletes)?;
        let written = write_config(&path, &text)?;
        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&current, "env.ANTHROPIC_BASE_URL");
        let token = json::get(&current, "env.ANTHROPIC_AUTH_TOKEN");
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = token.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let Ok(Some(current)) = read_config(&Self::settings_path(ctx)) else {
            return BTreeMap::new();
        };
        OUR_KEYS
            .iter()
            .filter_map(|key| json::get(&current, key).map(|value| ((*key).to_owned(), value)))
            .collect()
    }
}
