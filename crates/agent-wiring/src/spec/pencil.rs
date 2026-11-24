//! Pencil specification (`~/.pencil/models.json`).
//!
//! Pencil reads OpenAI-compatible providers from `providers.<id>`; each
//! provider carries the base URL, key, and a list of model descriptors. Wiring
//! registers `providers.any-converter` and leaves model selection to the user,
//! so no default model key is written.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    join_relative, read_config, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Pencil spec.
pub const OUR_KEYS: &[&str] = &["providers.any-converter"];

const PROVIDER: &str = "any-converter";
const PROVIDER_PATH: &str = "providers.any-converter";
/// Default context window when the catalog does not report one.
const DEFAULT_CONTEXT: u64 = 128_000;
/// Output cap Pencil accepts on every model.
const MAX_OUTPUT: u64 = 16_384;

/// Pencil wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct PencilSpec;

impl PencilSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        join_relative(&ctx.home, ".pencil")
    }

    fn models_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("models.json")
    }
}

impl AgentSpec for PencilSpec {
    fn id(&self) -> &'static str {
        "pencil"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir() || Self::models_path(ctx).exists() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::models_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::models_path(ctx);
        let current = read_config(&path)?.unwrap_or_default();
        let text = json::set(&current, &[KV::new(PROVIDER_PATH, provider(wiring))])?;
        let written = write_config(&path, &text)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::models_path(ctx);
        let mut written = Vec::new();
        if let Some(current) = read_config(&path)? {
            let text = json::del_pruning(&current, &[PROVIDER_PATH])?;
            written.push(write_config(&path, &text)?);
        }
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::models_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&current, "providers.any-converter.baseUrl");
        let key = json::get(&current, "providers.any-converter.apiKey");
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(current)) = read_config(&Self::models_path(ctx)) {
            for key in ["baseUrl", "apiKey"] {
                if let Some(value) = json::get(&current, &format!("{PROVIDER_PATH}.{key}")) {
                    values.insert(format!("{PROVIDER_PATH}.{key}"), value);
                }
            }
        }
        values
    }
}

/// The provider block Pencil reads.
fn provider(wiring: &WiringCtx<'_>) -> Value {
    let models: Vec<Value> = wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            let context = model.context_tokens.unwrap_or(DEFAULT_CONTEXT);
            json!({
                "id": model.id,
                "name": model.name,
                "reasoning": model.reasoning,
                // The catalog does not report modalities; text-only is the
                // conservative default.
                "input": ["text"],
                "compat": {},
                "cost": {
                    "input": 0,
                    "output": 0,
                    "cacheRead": 0,
                    "cacheWrite": 0,
                },
                "contextWindow": context,
                "maxTokens": context.min(MAX_OUTPUT),
            })
        })
        .collect();
    json!({
        "name": PROVIDER,
        "baseUrl": gateway_path(wiring.gateway_url, "v1"),
        "api": "openai-completions",
        "apiKey": wiring.agent_key,
        "models": models,
    })
}
