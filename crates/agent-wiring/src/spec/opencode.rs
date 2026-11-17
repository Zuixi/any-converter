//! OpenCode specification (`opencode.jsonc` in the OpenCode config dir).
//!
//! OpenCode loads OpenAI-compatible providers from `provider` (1.x) or
//! `providers` (2.x). Both shapes point at the gateway's `/v1` base URL with
//! the agent key, list the catalog under `models`, and select one through the
//! top-level `model` field (`<provider>/<model>`). Writing both shapes keeps a
//! single config working across the migration.
//!
//! The config file is chosen the way OpenCode chooses it: the first existing
//! file among `opencode.jsonc`, `opencode.json`, and `config.json`, otherwise
//! a fresh `opencode.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, env_dir, gateway_path,
    gw_base_match, load_stash, read_config, record_old_values, save_stash, write_config,
    xdg_config_home,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the OpenCode spec.
pub const OUR_KEYS: &[&str] = &[
    "provider.any-converter",
    "providers.any-converter",
    "model",
    "small_model",
];

/// Provider id written into both config shapes.
pub const PROVIDER_ID: &str = "any-converter";
const V1_PATH: &str = "provider.any-converter";
const V2_PATH: &str = "providers.any-converter";
const MODEL: &str = "model";
const SMALL_MODEL: &str = "small_model";
const CANDIDATES: &[&str] = &["opencode.jsonc", "opencode.json", "config.json"];

/// OpenCode wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenCodeSpec;

impl OpenCodeSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "OPENCODE_CONFIG_DIR", |ctx| {
            xdg_config_home(ctx).join("opencode")
        })
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        let dir = Self::dir(ctx);
        CANDIDATES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
            .unwrap_or_else(|| dir.join("opencode.json"))
    }
}

impl AgentSpec for OpenCodeSpec {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        let installed = config_dir.is_dir() || bin_in_path(ctx, "opencode");
        if installed {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::config_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let settings = read_config(&path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in [V1_PATH, V2_PATH, MODEL, SMALL_MODEL] {
            if let Some(value) = json::get(&settings, key) {
                old.insert(key.to_owned(), value);
            }
        }

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let base = gateway_path(wiring.gateway_url, "v1");
        let v1 = v1_provider(wiring, &base);
        let v2 = v2_provider(wiring, &base);
        let selected = format!("{PROVIDER_ID}/{}", wiring.default_model);

        let text = json::set(
            &settings,
            &[
                KV::new(V1_PATH, v1),
                KV::new(V2_PATH, v2),
                KV::new(MODEL, json!(selected)),
                KV::new(SMALL_MODEL, json!(selected)),
            ],
        )?;
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let mut text = json::del(&current, &[V1_PATH, V2_PATH])?;
            text = restore(&text, MODEL, old.get(MODEL))?;
            text = restore(&text, SMALL_MODEL, old.get(SMALL_MODEL))?;
            text = prune_empty(&text, "provider")?;
            text = prune_empty(&text, "providers")?;
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(settings)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let block = provider_block(&settings);
        let base = block
            .as_ref()
            .and_then(|value| {
                value
                    .pointer("/options/baseURL")
                    .or_else(|| value.pointer("/settings/baseURL"))
            })
            .and_then(Value::as_str);
        let key_present = block
            .as_ref()
            .and_then(|value| {
                value
                    .pointer("/options/apiKey")
                    .or_else(|| value.pointer("/settings/apiKey"))
            })
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        let selected = json::get(&settings, MODEL);
        let model_known = block
            .as_ref()
            .and_then(|value| value.get("models"))
            .and_then(Value::as_object)
            .is_some_and(|models| {
                selected
                    .as_deref()
                    .and_then(|id| id.split_once('/'))
                    .is_some_and(|(provider, model)| {
                        provider == PROVIDER_ID && models.contains_key(model)
                    })
            });

        if block.is_none() && selected.is_none() {
            return WiredState::Disconnected;
        }
        let gateway = base.is_some_and(|value| gw_base_match(value, gateway_url));
        if gateway && key_present && model_known {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::config_path(ctx)) {
            for path in [V1_PATH, MODEL] {
                if let Some(value) = json::get(&settings, path) {
                    values.insert(path.to_owned(), value);
                }
            }
        }
        values
    }
}

fn v1_provider(wiring: &WiringCtx<'_>, base: &str) -> Value {
    json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": "Any Converter",
        "options": {
            "baseURL": base,
            "apiKey": wiring.agent_key,
            "setCacheKey": true,
        },
        "models": models(wiring),
    })
}

fn v2_provider(wiring: &WiringCtx<'_>, base: &str) -> Value {
    json!({
        "name": "Any Converter",
        "package": "aisdk:@ai-sdk/openai-compatible",
        "settings": {
            "baseURL": base,
            "apiKey": wiring.agent_key,
        },
        "compatibility": { "supportsPromptCacheKey": true },
        "models": models(wiring),
    })
}

/// Model metadata keyed by catalog id; keying by id keeps user edits visible.
fn models(wiring: &WiringCtx<'_>) -> Value {
    let mut entries = serde_json::Map::new();
    for model in &wiring.catalog.models {
        let mut entry = serde_json::Map::new();
        entry.insert("name".to_owned(), json!(model.name));
        if let Some(context) = model.context_tokens {
            entry.insert(
                "limit".to_owned(),
                json!({ "context": context, "output": context.min(8192) }),
            );
        }
        entries.insert(model.id.clone(), Value::Object(entry));
    }
    Value::Object(entries)
}

/// Reads whichever provider shape the file uses.
fn provider_block(settings: &str) -> Option<Value> {
    let raw = json::get(settings, V2_PATH).or_else(|| json::get(settings, V1_PATH))?;
    serde_json::from_str(&raw).ok()
}

fn restore(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => json::set(text, &[KV::new(path, json!(value))])?,
        _ => json::del(text, &[path])?,
    })
}

fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match json::get(text, path) {
        Some(value) if value.trim() == "{}" => Ok(json::del(text, &[path])?),
        _ => Ok(text.to_owned()),
    }
}
